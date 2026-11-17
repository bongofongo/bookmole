-- Copies: which of a book you have, in what
-- form, on what terms, and where it sits. Membership of the library is derived
-- from this table (`storage/copies.rs`), never stored on `books`.
--
-- ---------------------------------------------------------------------------
-- `copies`
--
-- * `form` is NULLABLE. `file | print | app` are the three things a copy can
--   be; a back-filled book with no evidence at all (below) *is a copy of
--   unknown form*, and a fourth value `'unknown'` would be a lie in the same
--   column the other three are true in. NULL means "not said", which is what
--   `have_now` already means for NULL, and the engine's typed `Copy.form` is an
--   `Option<CopyForm>`.
-- * `have_now` is three-valued: 1, 0, or NULL = not said. Only an explicit 0
--   takes a copy out of the reckoning. **Only the user ever writes it** — no
--   importer, no scan, no passed `due_at` flips it.
-- * `started_at`, `due_at`, `ended_at` are user-entered and optional.
-- * Free text (`from_whom`, `app`, `location`, `lent_to`, `note`) is the user's
--   private writing; empty strings are stored as NULL by the engine.
--
-- `copy_places`
--
-- A copy is identified by its PLACE, not by its bytes: Preview
-- rewrites a PDF when it annotates it, so the hash is a fact about the last
-- look. PRIMARY KEY (kind, ref) makes a place belong to exactly one copy.
-- `ref` per kind (the format a later importer must keep):
--   calibre      the calibre book **uuid** (exactly what `external_ids`
--                holds under source 'calibre'). The library root and format go
--                in `detail`; the file's path is looked up again via calibredb.
--   reader       the KOReader `partial_md5`. `detail` is the paired device id
--                where known (the back-fill cannot know: `device_books` never
--                recorded it, see 0020).
--   path         an absolute path on this computer.
--   store        the sha256 of readingbuddy's own `database/files/…` file
--                (`book_files.sha256`).
--   apple_books  the Apple Books asset id (`ZASSETID`).
-- `last_hash` is the content hash the last time we looked; `last_mtime` is the
-- file's mtime then (a `path` place is rescanned when it changes);
-- `missing_since` is a FACT about the place, never a verdict on the copy.
--
-- `detail` is where a place keeps the second half of what identifies it
-- without widening the key.
-- ---------------------------------------------------------------------------
CREATE TABLE copies (
    id           INTEGER PRIMARY KEY,
    book_id      INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    form         TEXT CHECK (form IN ('file', 'print', 'app')),
    tenure       TEXT NOT NULL
                 CHECK (tenure IN ('own', 'borrowed', 'rented', 'subscription', 'unrecorded')),
    have_now     INTEGER CHECK (have_now IN (0, 1)),
    from_whom    TEXT,
    app          TEXT,
    location     TEXT,
    started_at   INTEGER,
    due_at       INTEGER,
    ended_at     INTEGER,
    lent_to      TEXT,
    unreadable   INTEGER NOT NULL DEFAULT 0 CHECK (unreadable IN (0, 1)),
    note         TEXT,
    learned_from TEXT NOT NULL
                 CHECK (learned_from IN ('calibre', 'koreader', 'file_import',
                                         'apple_books', 'kindle', 'user', 'migration')),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_copies_book ON copies(book_id);

CREATE TABLE copy_places (
    copy_id       INTEGER NOT NULL REFERENCES copies(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL
                  CHECK (kind IN ('calibre', 'reader', 'path', 'store', 'apple_books')),
    ref           TEXT NOT NULL,
    detail        TEXT,
    last_hash     TEXT,
    last_seen_at  INTEGER,
    last_mtime    INTEGER,
    missing_since INTEGER,
    PRIMARY KEY (kind, ref)
);
CREATE INDEX idx_copy_places_copy ON copy_places(copy_id);

-- ---------------------------------------------------------------------------
-- Back-fill: nothing vanishes from the library on upgrade.
--
-- Every back-filled copy has `have_now = NULL`. We cannot know that a calibre
-- file is still on disk or that the reader still holds it, and writing 1 would
-- be the migration saying something only the user may say. NULL keeps the book
-- in the library under the rule (a copy not explicitly ended), touched or not.
-- Back-filled copies are `tenure = 'own'` where there is a file (calibre's
-- files, a device's copy, our own store are the user's), and
-- `'unrecorded'` where nothing is known at all.
--
-- A book's "file copy" is the first copy created for it in this file; later
-- evidence (a reader, a store file) attaches to it as another place, because
-- one file is one copy wherever it sits.
-- ---------------------------------------------------------------------------

-- A. calibre: one copy per book, one place per calibre uuid.
INSERT INTO copies (book_id, form, tenure, have_now, learned_from, created_at, updated_at)
SELECT DISTINCT book_id, 'file', 'own', NULL, 'calibre',
       strftime('%s','now'), strftime('%s','now')
  FROM external_ids WHERE source = 'calibre';

-- B. a book KOReader knows, with no copy yet.
INSERT INTO copies (book_id, form, tenure, have_now, learned_from, created_at, updated_at)
SELECT DISTINCT book_id, 'file', 'own', NULL, 'koreader',
       strftime('%s','now'), strftime('%s','now')
  FROM device_books
 WHERE book_id NOT IN (SELECT book_id FROM copies);

-- C. a file readingbuddy stored, with no copy yet.
INSERT INTO copies (book_id, form, tenure, have_now, learned_from, created_at, updated_at)
SELECT DISTINCT book_id, 'file', 'own', NULL, 'file_import',
       strftime('%s','now'), strftime('%s','now')
  FROM book_files
 WHERE book_id NOT IN (SELECT book_id FROM copies);

-- Places on the book's file copy (the lowest copy id it has so far).
INSERT INTO copy_places (copy_id, kind, ref, last_seen_at)
SELECT (SELECT MIN(c.id) FROM copies c WHERE c.book_id = e.book_id),
       'calibre', e.external_id, e.created_at
  FROM external_ids e WHERE e.source = 'calibre';

INSERT INTO copy_places (copy_id, kind, ref, last_hash, last_seen_at)
SELECT (SELECT MIN(c.id) FROM copies c WHERE c.book_id = d.book_id),
       'reader', d.partial_md5, d.partial_md5, d.last_seen
  FROM device_books d;

INSERT INTO copy_places (copy_id, kind, ref, last_hash, last_seen_at)
SELECT (SELECT MIN(c.id) FROM copies c WHERE c.book_id = f.book_id),
       'store', f.sha256, f.sha256, f.added_at
  FROM book_files f;

-- D. no evidence at all: in the library before copies existed, and how is not
-- known. form NULL (not said), tenure 'unrecorded', have_now NULL.
INSERT INTO copies (book_id, form, tenure, have_now, learned_from, created_at, updated_at)
SELECT id, NULL, 'unrecorded', NULL, 'migration',
       strftime('%s','now'), strftime('%s','now')
  FROM books WHERE id NOT IN (SELECT book_id FROM copies);
