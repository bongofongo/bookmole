//! The notes surface on the facade: lists, threads, links, graph, parts, mentions, previews.
//! Note text is the reader's private writing: never log it above `trace!`.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;

use crate::Engine;
use crate::book::Book;
use crate::error::{EngineError, Result};
use crate::note_graph::{self, GraphEdge, GraphScope, NodeKey};
use crate::notes::{self, LinkContext, LinkDirection, NoteKind, NotePart, PartMarker};
use crate::storage::{
    Highlight, NewHighlight, NoteRecord, NoteRowFacts, NoteRowFilter, NoteScope, OutgoingLink,
    ReadCount, SearchHit, ko_datetime_to_unix,
};

pub const MAX_UTC_OFFSET_MINUTES: i32 = 18 * 60;

#[derive(Debug, Clone)]
pub struct NoteRow {
    pub note: NoteRecord,
    pub book: Option<Book>,
    pub heading: Option<String>,
    pub excerpt: String,
    pub parts: i64,
    /// The latest part's page, else the note's.
    pub latest_part_at: i64,
    pub latest_part_page: Option<i64>,
    pub links_out: i64,
    pub links_in: i64,
    /// Hub id of the note's thread; `None` when it links nothing and nothing links it.
    pub thread: Option<i64>,
    /// Which read of its book the note's reading is; `None` for a note naming no reading.
    pub read: Option<ReadCount>,
}

#[derive(Debug, Clone, Default)]
pub struct NoteRowQuery {
    pub kinds: Option<Vec<NoteKind>>,
    pub scope: NoteScope,
    pub ids: Option<Vec<i64>>,
    /// Unix seconds, exclusive.
    pub until: Option<i64>,
    /// Cursor: rows strictly after `(latest_part_at, id)` in list order; `until` alone can skip ties.
    pub after: Option<(i64, i64)>,
    /// Negative is no limit.
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub hub: NoteRow,
    pub notes: i64,
    /// Distinct, in order of first appearance among members newest first.
    pub books: Vec<Book>,
    /// Members whose latest part is at or after `since`; `0` without one.
    pub joined: i64,
    pub members: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct NoteLink {
    pub from: NoteRow,
    pub target_title: String,
    pub to: Option<NoteRecord>,
}

#[derive(Debug, Clone)]
pub struct GraphNodeView {
    pub key: String,
    /// `None` for an unwritten link.
    pub row: Option<NoteRow>,
    pub title: String,
    pub degree: i64,
}

#[derive(Debug, Clone)]
pub struct GraphEdgeView {
    pub from: String,
    pub to: String,
    pub kind: note_graph::GraphEdgeKind,
}

#[derive(Debug, Clone, Default)]
pub struct NoteGraphView {
    pub nodes: Vec<GraphNodeView>,
    pub edges: Vec<GraphEdgeView>,
}

#[derive(Debug, Clone, Default)]
pub struct LinkPreview {
    pub outgoing: Vec<OutgoingLink>,
    pub incoming: Vec<LinkContext>,
}

impl Engine {
    /// A page of note rows, newest part first. `book_id` and `reading_id` are alternatives.
    pub async fn note_rows(&self, q: &NoteRowQuery) -> Result<Vec<NoteRow>> {
        let facts = self
            .storage
            .note_row_facts(&NoteRowFilter {
                kinds: q
                    .kinds
                    .as_ref()
                    .map(|ks| ks.iter().map(|k| k.as_str().to_string()).collect()),
                scope: q.scope,
                ids: q.ids.clone(),
                until: q.until,
                after: q.after,
                limit: q.limit,
            })
            .await?;
        let index = self.thread_index().await?;
        self.rows_from_facts(facts, &index).await
    }

    /// Every UTC month some note's latest part falls in, newest first, as `(year, month)`.
    pub async fn note_periods(&self) -> Result<Vec<(i32, u8)>> {
        self.storage.note_periods().await
    }

    /// [`Engine::note_periods`] in a fixed offset `utc_offset_minutes` east of UTC; refused past ±18h.
    pub async fn note_periods_in(&self, utc_offset_minutes: i32) -> Result<Vec<(i32, u8)>> {
        if utc_offset_minutes.abs() > MAX_UTC_OFFSET_MINUTES {
            return Err(EngineError::InvalidInput(format!(
                "a UTC offset of {utc_offset_minutes} minutes is not a zone"
            )));
        }
        self.storage
            .note_periods_in(i64::from(utc_offset_minutes) * 60)
            .await
    }

    async fn thread_index(&self) -> Result<HashMap<i64, i64>> {
        let threads = self.all_threads().await?;
        Ok(note_graph::thread_index(&threads))
    }

    async fn all_threads(&self) -> Result<Vec<note_graph::Thread>> {
        let edges = self.storage.all_link_edges().await?;
        let latest: HashMap<i64, i64> = self
            .storage
            .note_stamps()
            .await?
            .into_iter()
            .map(|s| (s.id, s.latest_part_at))
            .collect();
        Ok(note_graph::threads(&edges, &latest))
    }

    async fn rows_from_facts(
        &self,
        facts: Vec<NoteRowFacts>,
        threads: &HashMap<i64, i64>,
    ) -> Result<Vec<NoteRow>> {
        let mut books: HashMap<i64, Option<Book>> = HashMap::new();
        let reading_ids: Vec<i64> = facts.iter().filter_map(|f| f.note.reading_id).collect();
        let counts = self.storage.read_counts(&reading_ids).await?;
        let mut out = Vec::with_capacity(facts.len());
        for f in facts {
            let created = f.note.created_at.map_or(0, |t| t.unix_timestamp());
            let parts = notes::note_parts(&f.body, created, f.note.page);
            let latest = notes::latest_part(&parts);
            let excerpt = latest.map(|p| notes::excerpt(&p.body)).unwrap_or_default();
            let latest_part_page = latest.and_then(|p| p.page).or(f.note.page);
            let heading = notes::row_heading(&f.note.kind, &f.note.title, &f.body);
            let book = self.book_face(&mut books, f.note.book_id).await?;
            out.push(NoteRow {
                thread: threads.get(&f.note.id).copied(),
                book,
                heading,
                excerpt,
                parts: f.parts,
                latest_part_at: f.latest_part_at,
                latest_part_page,
                links_out: f.links_out,
                links_in: f.links_in,
                read: f.note.reading_id.and_then(|r| counts.get(&r).copied()),
                note: f.note,
            });
        }
        Ok(out)
    }

    async fn rows_by_id(&self, ids: Vec<i64>) -> Result<HashMap<i64, NoteRow>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .note_rows(&NoteRowQuery {
                ids: Some(ids),
                limit: -1,
                ..Default::default()
            })
            .await?;
        Ok(rows.into_iter().map(|r| (r.note.id, r)).collect())
    }

    /// Threads containing any of `note_ids`, newest member first; `since` counts members at or after.
    pub async fn threads_for(
        &self,
        note_ids: &[i64],
        since: Option<i64>,
    ) -> Result<Vec<ThreadSummary>> {
        let wanted: HashSet<i64> = note_ids.iter().copied().collect();
        let stamps = self.storage.note_stamps().await?;
        let latest: HashMap<i64, i64> = stamps.iter().map(|s| (s.id, s.latest_part_at)).collect();
        let book_of: HashMap<i64, Option<i64>> = stamps.iter().map(|s| (s.id, s.book_id)).collect();
        let edges = self.storage.all_link_edges().await?;
        let all = note_graph::threads(&edges, &latest);
        let index = note_graph::thread_index(&all);
        let mut picked: Vec<note_graph::Thread> = all
            .into_iter()
            .filter(|t| t.members.iter().any(|m| wanted.contains(m)))
            .collect();
        let at = |id: &i64| latest.get(id).copied().unwrap_or(i64::MIN);
        picked.sort_by_key(|t| std::cmp::Reverse((at(&t.members[0]), t.hub)));

        let facts = self
            .storage
            .note_row_facts(&NoteRowFilter {
                ids: Some(picked.iter().map(|t| t.hub).collect()),
                limit: -1,
                ..Default::default()
            })
            .await?;
        let mut hubs: HashMap<i64, NoteRow> = self
            .rows_from_facts(facts, &index)
            .await?
            .into_iter()
            .map(|r| (r.note.id, r))
            .collect();

        let mut cache: HashMap<i64, Option<Book>> = HashMap::new();
        let mut out = Vec::with_capacity(picked.len());
        for t in picked {
            let Some(hub) = hubs.remove(&t.hub) else {
                continue; // deleted between the two reads
            };
            let mut seen = HashSet::new();
            let mut books = Vec::new();
            for m in &t.members {
                if let Some(Some(b)) = book_of.get(m)
                    && seen.insert(*b)
                    && let Some(book) = self.book_face(&mut cache, Some(*b)).await?
                {
                    books.push(book);
                }
            }
            let joined = since.map_or(0, |s| t.members.iter().filter(|m| at(m) >= s).count());
            out.push(ThreadSummary {
                hub,
                notes: t.members.len() as i64,
                books,
                joined: joined as i64,
                members: t.members,
            });
        }
        Ok(out)
    }

    /// Every wikilink written in these notes: newest source first, body order within a note;
    /// self-links are left out.
    pub async fn links_from_notes(&self, note_ids: &[i64]) -> Result<Vec<NoteLink>> {
        let written = self.storage.links_written(note_ids).await?;
        let mut by_source: HashMap<i64, Vec<(String, Option<NoteRecord>)>> = HashMap::new();
        for (from, title, to) in written {
            if to.as_ref().is_some_and(|n| n.id == from) {
                continue;
            }
            by_source.entry(from).or_default().push((title, to));
        }
        let rows = self
            .note_rows(&NoteRowQuery {
                ids: Some(by_source.keys().copied().collect()),
                limit: -1,
                ..Default::default()
            })
            .await?;
        let mut out = Vec::new();
        for row in rows {
            for (target_title, to) in by_source.remove(&row.note.id).unwrap_or_default() {
                out.push(NoteLink {
                    from: row.clone(),
                    target_title,
                    to,
                });
            }
        }
        Ok(out)
    }

    /// The graph a links view draws. `Around` takes `steps` of 1 or 2; a missing note is `NotFound`.
    pub async fn note_graph(&self, scope: GraphScope) -> Result<NoteGraphView> {
        let mut mentions = Vec::new();
        if let GraphScope::Around { note_id, steps } = scope {
            if !(1..=2).contains(&steps) {
                return Err(EngineError::InvalidInput(format!(
                    "a graph around a note reaches 1 or 2 steps, not {steps}"
                )));
            }
            self.storage
                .get_note(note_id)
                .await?
                .ok_or_else(|| EngineError::NotFound(format!("note id {note_id}")))?;
            mentions = self
                .unlinked_mentions(note_id, note_graph::MENTION_EDGE_CAP as i64)
                .await?
                .iter()
                .filter_map(|h| h.as_note().map(|n| n.id))
                .collect();
        }
        let edges = self.storage.all_link_edges().await?;
        let stamps = self.storage.note_stamps().await?;
        let graph = note_graph::build_graph(scope, &edges, &stamps, &mentions);

        let ids: Vec<i64> = graph
            .nodes
            .iter()
            .filter_map(|n| match n.key {
                NodeKey::Note(id) => Some(id),
                NodeKey::Title(_) => None,
            })
            .collect();
        let mut rows = self.rows_by_id(ids).await?;
        let nodes = graph
            .nodes
            .into_iter()
            .filter_map(|n| {
                let key = n.key.wire();
                match n.key {
                    NodeKey::Note(id) => {
                        let row = rows.remove(&id)?;
                        Some(GraphNodeView {
                            key,
                            title: row.note.title.clone(),
                            row: Some(row),
                            degree: n.degree,
                        })
                    }
                    NodeKey::Title(t) => Some(GraphNodeView {
                        key,
                        row: None,
                        title: n.link_text.unwrap_or(t),
                        degree: n.degree,
                    }),
                }
            })
            .collect();
        let edges = graph
            .edges
            .into_iter()
            .map(|GraphEdge { from, to, kind }| GraphEdgeView {
                from: from.wire(),
                to: to.wire(),
                kind,
            })
            .collect();
        Ok(NoteGraphView { nodes, edges })
    }

    async fn note_or_not_found(&self, note_id: i64) -> Result<NoteRecord> {
        self.storage
            .get_note(note_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("note id {note_id}")))
    }

    /// Part numbers (1-based, ascending) of a note matched by `query`: parts holding every term,
    /// else any, else none. `NotFound` for a missing note.
    pub async fn matched_parts(&self, note_id: i64, query: &str) -> Result<Vec<i64>> {
        let parts = self.note_parts(note_id).await?;
        let texts: Vec<(i64, &str)> = parts.iter().map(|p| (p.n, p.body.as_str())).collect();
        self.storage.matching_texts(query, &texts).await
    }

    /// A note's parts, read from its file (the origin). Part 1 takes the note's `created_at`, `page`.
    pub async fn note_parts(&self, note_id: i64) -> Result<Vec<NotePart>> {
        let note = self.note_or_not_found(note_id).await?;
        let body = self.note_body(&note)?;
        Ok(parts_of(&note, &body))
    }

    /// Appends a dated part. Refuses an empty body, a body holding a marker line, and a negative page.
    pub async fn continue_note(
        &self,
        note_id: i64,
        body: &str,
        page: Option<i64>,
    ) -> Result<Vec<NotePart>> {
        if body.trim().is_empty() {
            return Err(EngineError::InvalidInput(
                "a continuation needs some words".into(),
            ));
        }
        refuse_marker(body)?;
        if page.is_some_and(|p| p < 0) {
            return Err(EngineError::InvalidInput("a page is not negative".into()));
        }
        let note = self.note_or_not_found(note_id).await?;
        let current = self.note_body(&note)?;
        let n = notes::split_parts(&current).len() as i64 + 1;
        let marker = PartMarker {
            n,
            at: OffsetDateTime::now_utc().unix_timestamp(),
            page,
        };
        let next = notes::append_part(&current, marker, body);
        self.write_note_body(&note, &next).await?;
        Ok(parts_of(&note, &next))
    }

    /// Rewrites part `n` (1-based) only. `NotFound` for a missing part; `InvalidInput` for a marker.
    pub async fn update_note_part(
        &self,
        note_id: i64,
        n: i64,
        body: &str,
    ) -> Result<Vec<NotePart>> {
        refuse_marker(body)?;
        let note = self.note_or_not_found(note_id).await?;
        let current = self.note_body(&note)?;
        let next = usize::try_from(n - 1)
            .ok()
            .and_then(|i| notes::replace_part(&current, i, body))
            .ok_or_else(|| EngineError::NotFound(format!("part {n} of note {note_id}")))?;
        self.write_note_body(&note, &next).await?;
        Ok(parts_of(&note, &next))
    }

    async fn write_note_body(&self, note: &NoteRecord, body: &str) -> Result<()> {
        let file = self.note_path(note)?;
        let content = std::fs::read_to_string(&file)?;
        let (header, _) = notes::frontmatter_and_body(&content);
        let body = body.trim_end();
        std::fs::write(&file, format!("{header}{body}\n"))?;
        let links = notes::extract_wikilinks(body);
        self.storage
            .reindex_note(note.id, &note.title, body, &links)
            .await
    }

    /// Notes mentioning this one's title without linking it, in index rank, at most `limit`.
    /// Every hit is one `notes::find_mention` agrees with, so `link_mention` can honour it.
    pub async fn unlinked_mentions(&self, note_id: i64, limit: i64) -> Result<Vec<SearchHit>> {
        let note = self.note_or_not_found(note_id).await?;
        let body = self
            .storage
            .indexed_body(note_id)
            .await?
            .unwrap_or_default();
        let title = note.title.trim();
        if limit <= 0
            || title.chars().count() < 3
            || !notes::linkable_title(title)
            || notes::title_is_derived(&note.title, &body)
        {
            return Ok(Vec::new());
        }
        let mut excluded: HashSet<i64> = self
            .storage
            .linked_note_ids(note_id)
            .await?
            .into_iter()
            .collect();
        excluded.insert(note_id);
        Ok(self
            .storage
            .mention_candidates(title)
            .await?
            .into_iter()
            .filter(|(n, body, _)| {
                !excluded.contains(&n.id) && notes::find_mention(body, title).is_some()
            })
            .take(limit as usize)
            .map(|(note, _, snippet)| SearchHit::Note { note, snippet })
            .collect())
    }

    /// Links the first unlinked mention of `to`'s title in `from`'s body and reindexes `from`.
    /// `NotFound` when the phrase is gone; `InvalidInput` for a self-link or an unholdable title.
    pub async fn link_mention(&self, from_note_id: i64, to_note_id: i64) -> Result<()> {
        if from_note_id == to_note_id {
            return Err(EngineError::InvalidInput(
                "a note does not mention itself".into(),
            ));
        }
        let from = self.note_or_not_found(from_note_id).await?;
        let to = self.note_or_not_found(to_note_id).await?;
        if !notes::linkable_title(&to.title) {
            return Err(EngineError::InvalidInput(format!(
                "note {to_note_id}'s title cannot be written as a [[link]]"
            )));
        }
        let body = self.note_body(&from)?;
        let next = notes::link_first_mention(&body, &to.title).ok_or_else(|| {
            EngineError::NotFound(format!(
                "note {from_note_id} no longer mentions note {to_note_id} without a link"
            ))
        })?;
        self.write_note_body(&from, &next).await
    }

    /// A passage typed by hand: `source = 'user'`, no `pos0`, `ko_datetime` of now to the nanosecond.
    /// The instant is stored, not only hashed, so the identity can be recomputed from the row. Its
    /// nanoseconds keep two passages typed with the same words apart, since the hash names no book.
    pub async fn create_highlight(
        &self,
        book_id: i64,
        text: &str,
        page: Option<i64>,
        chapter: Option<&str>,
    ) -> Result<Highlight> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EngineError::InvalidInput(
                "a passage needs its words".into(),
            ));
        }
        self.storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
        let chapter = chapter.map(str::trim).filter(|c| !c.is_empty());
        for _ in 0..8 {
            let now = OffsetDateTime::now_utc();
            let stamp = format!(
                "{}.{:09}",
                now.format(time::macros::format_description!(
                    "[year]-[month]-[day] [hour]:[minute]:[second]"
                ))?,
                now.nanosecond()
            );
            let new = NewHighlight {
                text: text.to_string(),
                chapter: chapter.map(str::to_string),
                page,
                pos0: None,
                pos1: None,
                ko_datetime: Some(stamp),
                ko_datetime_updated: None,
                color: None,
                note: None,
                source: "user".into(),
            };
            if let Some(id) = self.storage.insert_highlight(book_id, &new).await? {
                self.storage.attribute_highlights(book_id).await?;
                return self
                    .storage
                    .get_highlight(id)
                    .await?
                    .ok_or_else(|| EngineError::NotFound(format!("highlight id {id}")));
            }
        }
        Err(EngineError::Other(
            "a typed passage could not be given an identity".into(),
        ))
    }

    pub async fn preview_links(&self, title: Option<&str>, body: &str) -> Result<LinkPreview> {
        let mut outgoing = Vec::new();
        for target in notes::extract_wikilinks(body) {
            let to = self.storage.resolve_title(&target).await?;
            outgoing.push(OutgoingLink {
                target_title: target,
                to,
            });
        }
        let name = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or_else(|| notes::heading_of(body));
        let mut incoming = Vec::new();
        if let Some(name) = name {
            let mut books: HashMap<i64, Option<Book>> = HashMap::new();
            for (target_title, source) in self.storage.dangling_links_to(name).await? {
                let paragraph = match self.note_body(&source) {
                    Ok(b) => notes::link_paragraph(&b, &target_title),
                    Err(e) => {
                        tracing::debug!(note_id = source.id, error = %e, "link source body unreadable");
                        None
                    }
                };
                let book = self.book_face(&mut books, source.book_id).await?;
                incoming.push(LinkContext {
                    direction: LinkDirection::Incoming,
                    target_title,
                    note: Some(source),
                    book,
                    paragraph,
                });
            }
        }
        Ok(LinkPreview { outgoing, incoming })
    }

    pub async fn get_highlight(&self, id: i64) -> Result<Option<Highlight>> {
        self.storage.get_highlight(id).await
    }

    pub async fn note_books(&self) -> Result<Vec<(Book, i64)>> {
        let edges = self.storage.all_link_edges().await?;
        let mut linked: HashSet<i64> = HashSet::new();
        for e in &edges {
            if e.to != Some(e.from) {
                linked.insert(e.from);
                if let Some(t) = e.to {
                    linked.insert(t);
                }
            }
        }
        let mut per_book: HashMap<i64, i64> = HashMap::new();
        for s in self.storage.note_stamps().await? {
            if let (true, Some(b)) = (linked.contains(&s.id), s.book_id) {
                *per_book.entry(b).or_default() += 1;
            }
        }
        let order = self.title_order().await?;
        let mut out = Vec::with_capacity(per_book.len());
        for (id, n) in per_book {
            if let Some(book) = self.storage.get_book(id).await? {
                out.push((book, n));
            }
        }
        let pos = |b: &Book| {
            order
                .get(&b.id.unwrap_or_default())
                .copied()
                .unwrap_or(i64::MAX)
        };
        out.sort_by(|(a, an), (b, bn)| bn.cmp(an).then(pos(a).cmp(&pos(b))));
        Ok(out)
    }

    /// A read's notes and passages, newest first; ties go to the higher id, a note before a passage.
    /// A negative `limit` is no limit; an unknown reading is `NotFound`.
    pub async fn read_marks(&self, reading_id: i64, limit: i64) -> Result<Vec<ReadMark>> {
        let reading = self
            .storage
            .get_reading(reading_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("reading id {reading_id}")))?;
        let windows: Vec<_> = self
            .storage
            .read_windows()
            .await?
            .into_iter()
            .filter(|w| w.book_id == reading.book_id)
            .collect();
        let notes: Vec<(i64, i64)> = self
            .storage
            .note_stamps_for_history()
            .await?
            .into_iter()
            .filter(|n| crate::history::attribute_note(n, &windows) == Some(reading_id))
            .map(|n| (n.id, n.at))
            .collect();
        let mut rows = self
            .rows_by_id(notes.iter().map(|(id, _)| *id).collect())
            .await?;
        let mut out: Vec<ReadMark> = notes
            .into_iter()
            .filter_map(|(id, at)| {
                Some(ReadMark::Note {
                    at,
                    row: Box::new(rows.remove(&id)?),
                })
            })
            .collect();
        for h in self.storage.highlights_for_reading(reading_id).await? {
            let at = h
                .ko_datetime
                .as_deref()
                .and_then(ko_datetime_to_unix)
                .unwrap_or(h.created_at);
            out.push(ReadMark::Passage {
                at,
                highlight: Box::new(h),
            });
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.order_key()));
        if limit >= 0 {
            out.truncate(limit as usize);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub enum ReadMark {
    Note {
        at: i64,
        row: Box<NoteRow>,
    },
    /// A passage; `at` is the device's stamp, else when it was stored.
    Passage {
        at: i64,
        highlight: Box<Highlight>,
    },
}

impl ReadMark {
    pub fn at(&self) -> i64 {
        match self {
            ReadMark::Note { at, .. } | ReadMark::Passage { at, .. } => *at,
        }
    }

    fn order_key(&self) -> (i64, u8, i64) {
        match self {
            ReadMark::Note { at, row } => (*at, 1, row.note.id),
            ReadMark::Passage { at, highlight } => (*at, 0, highlight.id),
        }
    }
}

fn parts_of(note: &NoteRecord, body: &str) -> Vec<NotePart> {
    let created = note.created_at.map_or(0, |t| t.unix_timestamp());
    notes::note_parts(body, created, note.page)
}

fn refuse_marker(body: &str) -> Result<()> {
    if notes::contains_marker(body) {
        return Err(EngineError::InvalidInput(
            "a part's text cannot hold a part marker line".into(),
        ));
    }
    Ok(())
}
