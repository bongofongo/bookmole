-- The files a place holds.
--
-- A `calibre` place (0027) is one calibre book, and one calibre book is several
-- files — one per format. `copy_places.detail` kept only
-- `<library root>\t<formats>`, and the absolute path of each format, which
-- `calibredb list` hands over on every scan, was thrown away. This table keeps
-- it, per place.
--
-- ---------------------------------------------------------------------------
-- `copy_place_files`
--
-- * Keyed on the PLACE `(kind, ref)`, not on the copy: identity is the place
--   (0027), so merging two copies (`merge_copies` moves `copy_places.copy_id`
--   and never the key) carries these rows with no write at all, and deleting a
--   place or its copy cascades through `copy_places` to here.
-- * `kind`/`ref` are exactly `copy_places`' (same reference format per kind).
--   Today only `calibre` places have rows: a `path` place IS its file (the
--   reference is the path) and a `store` place's file is derived from
--   `book_files`, so neither needs one. A later place that also holds several
--   files writes here under its own kind.
-- * `format` is lowercased, as `files::format_of` derives it from the
--   extension (`epub`, `azw3`, `pdf`; `bin` when there is none).
-- * `path` is the absolute path on this computer **as the origin reported it
--   at the last scan** — a fact about that scan, never re-derived. Whether the
--   file is still there is asked of the disk when it matters (nothing here
--   records it), the same honesty as `missing_since` being about the place.
-- * The scan REPLACES a place's rows: a format calibre no longer lists is gone
--   from here, because calibre owns the list.
--
-- No back-fill (the repo's deliberate non-back-fill, again): the paths of a
-- place scanned before this migration were never recorded, and `detail`'s
-- root plus a format is not a path — calibre names the folder and the file
-- after the title and author it had at the time. Such a place has no rows
-- until the next calibre scan, and a reader of this table says "in calibre"
-- rather than inventing a path.
-- ---------------------------------------------------------------------------
CREATE TABLE copy_place_files (
    kind     TEXT NOT NULL,
    ref      TEXT NOT NULL,
    format   TEXT NOT NULL,
    path     TEXT NOT NULL,
    PRIMARY KEY (kind, ref, path),
    FOREIGN KEY (kind, ref) REFERENCES copy_places(kind, ref) ON DELETE CASCADE
);
