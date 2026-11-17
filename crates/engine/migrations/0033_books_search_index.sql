-- A ranked search over the books themselves.
--
-- Two searches stand on this: a search of the library (your books, nothing
-- sent anywhere) and the local half of a metadata search, which offers every
-- book the app already holds, in the library or not, before any provider's
-- answer. `Storage::search_books` reads it.
--
-- **Beside `BookFilter::title`, not instead of it.** That field is a `LIKE`,
-- and its doc comment's argument stands: it is a *filter* that composes with
-- five sorts, a count and offset paging through one `WHERE`, and it has to
-- match the way its five neighbours do. This is a *ranked search* behind its
-- own call, which is a different question. The one thing the comment prized —
-- infix, `possess` finding *The Dispossessed* — is exactly what the **trigram**
-- tokenizer keeps, where `unicode61`'s word tokens would have lost it.
--
-- `0015`'s argument, a third time: these are **columns**, so a trigger can read
-- them, and an *external-content* table indexes them without a second copy.
-- What that buys is that no writer of `books` can skip the index —
-- `upsert_book`, `enrich_book`, `fill_book`, `set_book_fields`, `merge_books`
-- (its fill of `dst` and its delete of `src`), `delete_book`, `refresh_sort_keys`
-- and `crates/corpus`' raw-SQL `gen-devdb` all keep it true without naming it.
--
-- What is indexed, and what is not:
--
-- * `title`, `authors`, `series`, `isbn_13`, `isbn_10` — every column a reader
--   identifies a book by. There is **no `subtitle` column** to index: a
--   subtitle arrives inside `title` (`Frankenstein: Or, The Modern
--   Prometheus`), so it is indexed there.
-- * `authors` is the stored JSON array as text. Its brackets and quotes make a
--   few trigrams nobody types, and that is all they do; indexing a cleaned copy
--   would need a column that does not exist or a contentless table, which
--   cannot `'rebuild'`.
-- * `remove_diacritics 1`, as `0032`'s `provider_record_index`: `bronte` finds
--   *Brontë*. Case is folded by the trigram tokenizer's default.
--
-- `AFTER UPDATE OF` the five columns, not every update: `refresh_sort_keys`
-- rewrites `sort_title`/`sort_author` inside every merge writer's transaction,
-- `set_cover` writes five cover columns, and `last_modified` moves on nearly
-- every write — none of those touches an indexed column, and an unrestricted
-- trigger would make each one pay a delete-and-reinsert of the index for
-- nothing (`0015`, point 3).
--
-- A book is deleted by `delete_book` and by `merge_books`' delete of `src`;
-- both fire the delete trigger directly (nothing cascades *into* `books`).

CREATE VIRTUAL TABLE books_search USING fts5(
    title,
    authors,
    series,
    isbn_13,
    isbn_10,
    content='books',
    content_rowid='id',
    tokenize='trigram remove_diacritics 1'
);

CREATE TRIGGER books_search_ai AFTER INSERT ON books BEGIN
    INSERT INTO books_search (rowid, title, authors, series, isbn_13, isbn_10)
    VALUES (new.id, new.title, new.authors, new.series, new.isbn_13, new.isbn_10);
END;

CREATE TRIGGER books_search_ad AFTER DELETE ON books BEGIN
    INSERT INTO books_search (books_search, rowid, title, authors, series, isbn_13, isbn_10)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.isbn_13, old.isbn_10);
END;

CREATE TRIGGER books_search_au
AFTER UPDATE OF title, authors, series, isbn_13, isbn_10 ON books BEGIN
    INSERT INTO books_search (books_search, rowid, title, authors, series, isbn_13, isbn_10)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.isbn_13, old.isbn_10);
    INSERT INTO books_search (rowid, title, authors, series, isbn_13, isbn_10)
    VALUES (new.id, new.title, new.authors, new.series, new.isbn_13, new.isbn_10);
END;

-- Every book from before this migration, indexed from the columns that
-- already hold it: the one-statement back-fill external content gets for free.
INSERT INTO books_search (books_search) VALUES ('rebuild');
