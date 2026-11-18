-- A note's body as a plain row, one per note.
--
-- The markdown file in the vault stays the origin of a note's body. This table
-- is a copy of the body as last indexed, the same text `notes_fts` holds, kept
-- where an ordinary query (and anything that exchanges rows) can read it
-- without going through the full-text index.
--
-- It has one writer: the code that indexes a note's body. That code upserts
-- here only when the body differs from the stored one (trailing whitespace is
-- not a difference), so an unchanged body leaves the row untouched and a write
-- here always means the body changed. That holds for AFTER triggers: a BEFORE
-- INSERT trigger fires on every upsert, ahead of its conflict check.
-- A row goes with its note by the cascade.
CREATE TABLE note_bodies (
    note_id INTEGER PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    body    TEXT NOT NULL
);

-- Back-fill from the index, which already holds every indexed body. A note
-- with no index row (or a NULL body in it) gets no row here rather than an
-- empty one: its body is unknown until its file is next indexed.
INSERT INTO note_bodies (note_id, body)
SELECT n.id, f.body
  FROM notes n
  JOIN notes_fts f ON f.rowid = n.id
 WHERE f.body IS NOT NULL;
