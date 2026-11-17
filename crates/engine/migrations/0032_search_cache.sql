-- Kept provider answers, kept searches, and which providers are switched on.
--
-- Four tables, and the line between them is one rule: **provider
-- records are kept long; searches are kept short.** A record is a fact about
-- the world (Open Library's work OL893415W is *Dune*); a search is a fact about
-- the reader (they looked for it). The first is kept for as long as the
-- provider's terms allow, the second for thirty days, as a hash.
--
-- ---------------------------------------------------------------------------
-- `provider_records`: one record a provider sent, as it sent it.
--
-- * Keyed `(provider, key)`, where `key` is the provider's own id: an Open
--   Library work (`/works/OL…W`), the edition it matched (`/books/OL…M`), or
--   `isbn:<13 digits>` for an edition looked up by ISBN.
-- * `kind` is `work` or `edition`. A search result is a work with the one
--   edition the provider matched to the query, kept as **two** records, because
--   the same work comes back under different queries with a different edition
--   matched each time: kept as one, the second search would overwrite the
--   first's edition and a repeat of the first would come back with the wrong
--   ISBN on it.
-- * `form` says how `body` is to be read: `doc` is the provider's own JSON for
--   that record, byte for byte; `book` is the engine's own reading of an answer
--   that took several requests to assemble (an Open Library ISBN lookup is the
--   edition, its authors and its work), where there is no single response to
--   keep. A body that no longer reads is dropped and asked for again
--   (`DiagnosticKind::KeptRecordUnreadable`).
-- * `title`/`authors` are copied out of the body for the index below and are
--   read by nothing else. Catalogue data, not the reader's.
-- * `expires_at` is `fetched_at` plus the provider's keep (Open Library: 180
--   days). Expired rows are deleted at open and on every write.
-- * **Google Books, Hardcover and iTunes never reach this table**:
--   their terms allow no durable copy. That is enforced in Rust — the
--   provider defaults table has no keep for them and the writer refuses — and
--   asserted on the tables by a test.
-- * No `provider` CHECK: the vocabulary grows with every provider added,
--   and `readings.source` (0005) and `field_provenance.source` (0012) set the
--   precedent of a vocabulary documented rather than constrained.
-- ---------------------------------------------------------------------------
CREATE TABLE provider_records (
    id          INTEGER PRIMARY KEY,
    provider    TEXT    NOT NULL,
    key         TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('work', 'edition')),
    form        TEXT    NOT NULL CHECK (form IN ('doc', 'book')),
    title       TEXT,
    authors     TEXT,
    body        TEXT    NOT NULL,
    fetched_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    UNIQUE (provider, key)
);

CREATE INDEX idx_provider_records_expires ON provider_records (expires_at);

-- ---------------------------------------------------------------------------
-- `provider_record_index`: so an offline search finds a book seen before by its
-- own words. External content over `provider_records`, kept current by
-- triggers — 0015's argument: these are columns, so a trigger can read them,
-- and no writer can forget the index. **Trigram** with diacritics removed, so a
-- part of a word (`possess`) and an unaccented one (`cafe`) both find it; a
-- query under three characters cannot use a trigram index and finds nothing
-- here, which is the right answer for an offline guess.
-- ---------------------------------------------------------------------------
CREATE VIRTUAL TABLE provider_record_index USING fts5(
    title,
    authors,
    content='provider_records',
    content_rowid='id',
    tokenize='trigram remove_diacritics 1'
);

CREATE TRIGGER provider_record_index_ai AFTER INSERT ON provider_records BEGIN
    INSERT INTO provider_record_index (rowid, title, authors)
    VALUES (new.id, new.title, new.authors);
END;

CREATE TRIGGER provider_record_index_ad AFTER DELETE ON provider_records BEGIN
    INSERT INTO provider_record_index (provider_record_index, rowid, title, authors)
    VALUES ('delete', old.id, old.title, old.authors);
END;

CREATE TRIGGER provider_record_index_au
AFTER UPDATE OF title, authors ON provider_records BEGIN
    INSERT INTO provider_record_index (provider_record_index, rowid, title, authors)
    VALUES ('delete', old.id, old.title, old.authors);
    INSERT INTO provider_record_index (rowid, title, authors)
    VALUES (new.id, new.title, new.authors);
END;

-- ---------------------------------------------------------------------------
-- `search_cache`: what a search returned, per provider.
--
-- * `query_key` is the hex sha256 of the folded query and the shape it was
--   read as (`search::session::query_key`), so a repeat search is recognised
--   **without the database holding a readable history**. It is
--   not salted, and that is a stated limit rather than an oversight: anybody
--   holding this file can test a guess ("did they look for *Dune*?") but cannot
--   list what was searched, and a salt stored beside the hashes would change
--   neither half of that.
-- * `results` is a JSON array, in the provider's order, naming the
--   `provider_records` each result was kept as. Never a title, never the text
--   typed.
-- * Rows older than thirty days are deleted at open and on every write.
--   Forgetting searches empties this table; forgetting kept records as well
--   empties the other two. Neither touches a book anybody added.
-- ---------------------------------------------------------------------------
CREATE TABLE search_cache (
    query_key   TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    results     TEXT    NOT NULL,
    fetched_at  INTEGER NOT NULL,
    PRIMARY KEY (query_key, provider)
) WITHOUT ROWID;

CREATE INDEX idx_search_cache_fetched ON search_cache (fetched_at);

-- ---------------------------------------------------------------------------
-- `provider_settings`: a provider switched on or off by the reader.
--
-- **Interpreted by the engine**, so it is its own table and not the frontend's
-- settings store (0024), which stores and never interprets. A provider with no
-- row has its default from the defaults table in `providers/mod.rs` (keyless
-- providers on, keyed ones on only once a key exists), so **nothing is seeded
-- here**: a seeded row would freeze today's default into every library, and a
-- provider added later (S5, S8) needs no migration to have one. No secret goes
-- in this table — keys and tokens keep their own paths.
-- ---------------------------------------------------------------------------
CREATE TABLE provider_settings (
    provider    TEXT    PRIMARY KEY,
    enabled     INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_at  INTEGER NOT NULL
);

-- `field_provenance.source` (0012) has no CHECK, so providers added later
-- need nothing here to be attributable.
