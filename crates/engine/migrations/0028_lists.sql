-- Lists: named, ordered collections of books, each entry with an optional
-- note. Collections had been deferred because three systems minting them
-- (Goodreads shelves, calibre tags, the app itself) is a merge problem with no
-- good default. A list sidesteps it by being minted in one place only: here,
-- by the reader.
--
-- Not a back-fill and not a rewrite of `book_tags`: Goodreads shelves stay in
-- `book_tags` as inert provenance and are only *offered* as lists. Keeping one
-- copies its books into a list; nothing joins on `source_ref` afterwards.

CREATE TABLE book_lists (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,                 -- NULL = none; never ''
    -- Where the list came from: 'user', or 'goodreads' for a kept shelf.
    source      TEXT NOT NULL DEFAULT 'user',
    -- For a kept shelf: the shelf's tag as stored in book_tags.tag.
    source_ref  TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL      -- any change to the list or its entries
);

CREATE TABLE book_list_entries (
    list_id  INTEGER NOT NULL REFERENCES book_lists(id) ON DELETE CASCADE,
    book_id  INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,        -- 0-based, contiguous within a list
    note     TEXT,                    -- NULL = no note; never ''
    added_at INTEGER NOT NULL,
    PRIMARY KEY (list_id, book_id)
);
CREATE INDEX idx_book_list_entries_book ON book_list_entries(book_id);
CREATE UNIQUE INDEX idx_book_list_entries_pos ON book_list_entries(list_id, position);

-- A suggestion the reader said "Not now" to, so the engine does not offer it
-- again on every visit and no frontend has to filter.
CREATE TABLE dismissed_shelves (
    source       TEXT NOT NULL,       -- 'goodreads'
    tag          TEXT NOT NULL,
    dismissed_at INTEGER NOT NULL,
    PRIMARY KEY (source, tag)
);
