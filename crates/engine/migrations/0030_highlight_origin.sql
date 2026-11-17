-- Every highlight records where it came from.
--
-- `origin` is the typed source of a highlight; `source` (0001) stays, beside it,
-- and is NOT dropped here. Decision: `origin` is what new code reads and
-- branches on; `source` is still written, with the same word for the two origins
-- that exist today (`koreader`, `user`), so a raw reader of the old column keeps
-- working until a later migration retires it. Nothing in the engine reads
-- `source` for a decision after this.
--
-- ---------------------------------------------------------------------------
-- The `ko_` pair is superseded, not dropped (back-fill first, drop second, the
-- order every destructive migration here follows). `origin_note` /
-- `last_seen_origin_note` are back-filled
-- from `ko_note` / `last_seen_ko_note` below, all code moves to the new pair,
-- and the engine STOPS WRITING the old ones — nothing reads them, so keeping
-- them in step would be a second source of truth with no reader. They go in a
-- later migration, once this has settled.
--
-- Ownership (0004) generalises: whatever the source app wrote — text, its note,
-- its colour — belongs to that app; `annotation` is readingbuddy's own and an
-- import never touches it.
-- ---------------------------------------------------------------------------

-- The index first: external-content fts5 cannot rename a column, and its
-- triggers name `ko_note`. Drop triggers, then the table; recreated below over
-- the new column and rebuilt from the content table.
DROP TRIGGER highlights_fts_ai;
DROP TRIGGER highlights_fts_ad;
DROP TRIGGER highlights_fts_au;
DROP TABLE highlights_fts;

ALTER TABLE highlights ADD COLUMN origin TEXT
    CHECK (origin IN ('koreader', 'preview', 'apple_books', 'kindle', 'kindle_app', 'user'));
-- The copy it was made in. NULL is history with no copy (or no unambiguous one).
ALTER TABLE highlights ADD COLUMN copy_id INTEGER REFERENCES copies(id) ON DELETE SET NULL;
-- The source's own stable id (Apple Books' annotation uuid). NULL for KOReader,
-- which keeps using `identity_hash`.
ALTER TABLE highlights ADD COLUMN origin_id TEXT;
ALTER TABLE highlights ADD COLUMN origin_note TEXT;
ALTER TABLE highlights ADD COLUMN last_seen_origin_note TEXT;
-- Positions are kept in the source's own unit and never converted.
ALTER TABLE highlights ADD COLUMN pos_kind TEXT
    CHECK (pos_kind IN ('koreader_xpointer', 'kindle_location', 'epub_cfi', 'pdf_rect'));
-- A later PDF importer keeps a highlight whose words could not be extracted.
ALTER TABLE highlights ADD COLUMN text_unavailable INTEGER NOT NULL DEFAULT 0
    CHECK (text_unavailable IN (0, 1));

-- Back-fill. `source = 'user'` rows (0023) are the user's; everything else that
-- was ever imported came from KOReader — it is the only importer there has been.
UPDATE highlights
   SET origin = CASE WHEN source = 'user' THEN 'user' ELSE 'koreader' END,
       origin_note = ko_note,
       last_seen_origin_note = last_seen_ko_note;

UPDATE highlights SET pos_kind = 'koreader_xpointer'
 WHERE origin = 'koreader' AND pos0 IS NOT NULL;

-- copy_id only where it is unambiguous: a KOReader highlight whose book has
-- exactly one copy holding a `reader` place (0027 gave every `device_books`
-- row one). Two such copies — or none — is a guess, so it stays NULL.
UPDATE highlights
   SET copy_id = (SELECT c.id FROM copies c
                   WHERE c.book_id = highlights.book_id
                     AND EXISTS (SELECT 1 FROM copy_places p
                                  WHERE p.copy_id = c.id AND p.kind = 'reader'))
 WHERE origin = 'koreader'
   AND (SELECT count(*) FROM copies c
         WHERE c.book_id = highlights.book_id
           AND EXISTS (SELECT 1 FROM copy_places p
                        WHERE p.copy_id = c.id AND p.kind = 'reader')) = 1;

CREATE INDEX idx_highlights_copy ON highlights(copy_id);

-- The search index, over the new column. Same four properties as 0015
-- (external content, one-statement back-fill, `AFTER UPDATE OF`, a cascade
-- fires the delete trigger).
CREATE VIRTUAL TABLE highlights_fts USING fts5(
    text,
    origin_note,
    annotation,
    content='highlights',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE TRIGGER highlights_fts_ai AFTER INSERT ON highlights BEGIN
    INSERT INTO highlights_fts (rowid, text, origin_note, annotation)
    VALUES (new.id, new.text, new.origin_note, new.annotation);
END;

CREATE TRIGGER highlights_fts_ad AFTER DELETE ON highlights BEGIN
    INSERT INTO highlights_fts (highlights_fts, rowid, text, origin_note, annotation)
    VALUES ('delete', old.id, old.text, old.origin_note, old.annotation);
END;

CREATE TRIGGER highlights_fts_au
AFTER UPDATE OF text, origin_note, annotation ON highlights BEGIN
    INSERT INTO highlights_fts (highlights_fts, rowid, text, origin_note, annotation)
    VALUES ('delete', old.id, old.text, old.origin_note, old.annotation);
    INSERT INTO highlights_fts (rowid, text, origin_note, annotation)
    VALUES (new.id, new.text, new.origin_note, new.annotation);
END;

INSERT INTO highlights_fts (highlights_fts) VALUES ('rebuild');
