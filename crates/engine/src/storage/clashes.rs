//! Rows from two devices that are each valid alone and clash together: one open reading per book,
//! one reflection and one review per reading, one default rating scale. The kernel holds back the
//! row that would break the rule, and a merge of two book rows that would leave two open readings
//! on one. Every device decides each clash the same way from the same
//! facts, and only a device whose row in the table loses changes it ([`decides_here`]), which
//! lets the held row in and, once sent, tells the other devices. A row's fields are sent in
//! registry order, and a rule waits for a field declared after every field it reads, so it never
//! decides on half an edit.

use std::collections::{BTreeMap, BTreeSet};

use fongo_sync_sqlite::core::{Gid, Value};
use fongo_sync_sqlite::{Held, Replica};
use sqlx::SqliteConnection;

use super::synced::ids;
use crate::error::Result;
use crate::sync::kernel;

/// A held entity's merged values. A field that has not arrived is absent; so is one never set.
type Facts = BTreeMap<String, Value>;

/// Gives up each row of this table that keeps a held row out and loses to it, where this device
/// decides. Returns whether it changed any, so the caller knows to try the held rows again.
pub(crate) async fn give_way(
    conn: &mut SqliteConnection,
    replica: &Replica,
    held: &[Held],
) -> Result<bool> {
    // Two book rows becoming one wait while each has an open reading: the book is held under
    // both keys, and the reading that would move is held under its own.
    let mut merging: BTreeMap<Gid, BTreeSet<i64>> = BTreeMap::new();
    for h in held.iter().filter(|h| h.collection == "books") {
        if let Some(id) = h.key.as_deref().and_then(first_id) {
            merging.entry(h.gid).or_default().insert(id);
        }
    }
    let mut losers = BTreeSet::new();
    for rows in merging.values().filter(|rows| rows.len() > 1) {
        losers.extend(open_readings(conn, replica, rows, None).await?);
    }
    for h in held {
        if !matches!(
            h.collection.as_str(),
            "readings" | "notes" | "rating_scales"
        ) {
            continue;
        }
        let facts = replica
            .merged(&mut *conn, &h.collection, h.gid)
            .await
            .map_err(kernel)?;
        let held = Clashing { gid: h.gid, facts };
        match h.collection.as_str() {
            "readings" => losers.extend(open_reading(conn, replica, &held).await?),
            "notes" => losers.extend(one_per_reading(conn, replica, &held).await?),
            _ => losers.extend(default_scale(conn, replica, &held).await?),
        }
    }
    let mut changed = false;
    for loser in &losers {
        if decides_here(conn, replica, loser).await? {
            give_up(conn, loser).await?;
            changed = true;
        }
    }
    Ok(changed)
}

/// A row in this table that loses a clash.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Loser {
    rule: Rule,
    id: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    OpenReading,
    OnePerReading,
    DefaultScale,
}

/// Whether this device gives up a losing row it holds. Every device holding it does, though one
/// that only received it may decide after a reader's later edit elsewhere and overrule it: only
/// the device that made the row should, and the kernel does not yet say which that is.
async fn decides_here(
    _conn: &mut SqliteConnection,
    _replica: &Replica,
    _loser: &Loser,
) -> Result<bool> {
    Ok(true)
}

async fn give_up(conn: &mut SqliteConnection, loser: &Loser) -> Result<()> {
    let sql = match loser.rule {
        Rule::OpenReading => return super::readings::give_up_open_reading(conn, loser.id).await,
        Rule::OnePerReading => "UPDATE notes SET kind = 'note' WHERE id = ?",
        Rule::DefaultScale => "UPDATE rating_scales SET is_default = 0 WHERE id = ?",
    };
    sqlx::query(sql).bind(loser.id).execute(conn).await?;
    Ok(())
}

fn first_id(key: &str) -> Option<i64> {
    ids(key).first().copied()
}

struct Clashing {
    gid: Gid,
    facts: Facts,
}

impl Clashing {
    fn int(&self, field: &str) -> Option<i64> {
        match self.facts.get(field) {
            Some(Value::Int(v)) => Some(*v),
            _ => None,
        }
    }

    fn text(&self, field: &str) -> Option<&str> {
        match self.facts.get(field) {
            Some(Value::Text(v)) => Some(v),
            _ => None,
        }
    }

    /// The key here of the entity a reference field names.
    async fn named(
        &self,
        conn: &mut SqliteConnection,
        replica: &Replica,
        field: &str,
        collection: &str,
    ) -> Result<Option<String>> {
        let Some(Value::Ref(gid)) = self.facts.get(field) else {
            return Ok(None);
        };
        replica
            .key_for_gid(conn, collection, *gid)
            .await
            .map_err(kernel)
    }
}

async fn gid_of(
    conn: &mut SqliteConnection,
    replica: &Replica,
    collection: &str,
    id: i64,
) -> Result<Option<Gid>> {
    replica.gid_of(conn, collection, id).await.map_err(kernel)
}

/// An open reading held back from a book that has an open reading here.
async fn open_reading(
    conn: &mut SqliteConnection,
    replica: &Replica,
    held: &Clashing,
) -> Result<Vec<Loser>> {
    let Some(created) = held.int("created_at") else {
        return Ok(Vec::new());
    };
    if held
        .facts
        .get("finished_at")
        .is_some_and(|v| *v != Value::Null)
    {
        return Ok(Vec::new());
    }
    let Some(book) = held
        .named(conn, replica, "book_id", "books")
        .await?
        .as_deref()
        .and_then(first_id)
    else {
        return Ok(Vec::new());
    };
    open_readings(
        conn,
        replica,
        &BTreeSet::from([book]),
        Some((created, held.gid)),
    )
    .await
}

/// Of the open readings on the book rows `rows`, and `held` if given, the one made last stays open
/// and the others here are put down, as when two books become one. Not by when each began: a reader may
/// move that while two cross, and two devices would decide on different starts.
async fn open_readings(
    conn: &mut SqliteConnection,
    replica: &Replica,
    rows: &BTreeSet<i64>,
    held: Option<(i64, Gid)>,
) -> Result<Vec<Loser>> {
    let mut open = Vec::new();
    for &book in rows {
        let here: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT id, created_at FROM readings WHERE book_id = ? AND finished_at IS NULL",
        )
        .bind(book)
        .fetch_all(&mut *conn)
        .await?;
        for (id, made) in here {
            if let Some(gid) = gid_of(conn, replica, "readings", id).await? {
                open.push(((made, gid), id));
            }
        }
    }
    let Some(newest) = open.iter().map(|(age, _)| *age).chain(held).max() else {
        return Ok(Vec::new());
    };
    Ok(open
        .into_iter()
        .filter(|(age, _)| *age < newest)
        .map(|(_, id)| Loser {
            rule: Rule::OpenReading,
            id,
        })
        .collect())
}

/// Of two reflections, or two reviews, of one reading, the later becomes a plain note. Readings
/// never become one, so two reviews meet only through a held one.
async fn one_per_reading(
    conn: &mut SqliteConnection,
    replica: &Replica,
    held: &Clashing,
) -> Result<Option<Loser>> {
    let Some(created) = held.int("created_at") else {
        return Ok(None);
    };
    let Some(kind) = held
        .text("kind")
        .filter(|k| matches!(*k, "reflection" | "review"))
    else {
        return Ok(None);
    };
    let Some(reading) = held
        .named(conn, replica, "reading_id", "readings")
        .await?
        .as_deref()
        .and_then(first_id)
    else {
        return Ok(None);
    };
    let here: Option<(i64, i64)> =
        sqlx::query_as("SELECT id, created_at FROM notes WHERE reading_id = ?1 AND kind = ?2")
            .bind(reading)
            .bind(kind)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((id, made)) = here else {
        return Ok(None);
    };
    let Some(gid) = gid_of(conn, replica, "notes", id).await? else {
        return Ok(None);
    };
    Ok(((made, gid) > (created, held.gid)).then_some(Loser {
        rule: Rule::OnePerReading,
        id,
    }))
}

/// Of two default scales, the older stops being the default. Only a new scale becomes the
/// default, and the seeded `default` scale exists on every device, made whenever that device's
/// library was, so it counts as older than any scale a reader made.
async fn default_scale(
    conn: &mut SqliteConnection,
    replica: &Replica,
    held: &Clashing,
) -> Result<Option<Loser>> {
    if held.int("is_default") != Some(1) {
        return Ok(None);
    }
    let (Some(created), Some(name)) = (held.int("created_at"), held.text("name")) else {
        return Ok(None);
    };
    let here: Option<(i64, String, i64)> =
        sqlx::query_as("SELECT id, name, created_at FROM rating_scales WHERE is_default = 1")
            .fetch_optional(&mut *conn)
            .await?;
    let Some((id, here_name, made)) = here else {
        return Ok(None);
    };
    let Some(gid) = gid_of(conn, replica, "rating_scales", id).await? else {
        return Ok(None);
    };
    let age = |name: &str, created: i64, gid: Gid| (name != "default", created, gid);
    Ok(
        (age(&here_name, made, gid) < age(name, created, held.gid)).then_some(Loser {
            rule: Rule::DefaultScale,
            id,
        }),
    )
}
