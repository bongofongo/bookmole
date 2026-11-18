-- A list's positions are no longer forced unique by the database.
--
-- 0028 made `(list_id, position)` a unique index. Two changes to one list made
-- apart (say, a book appended on each of two copies of the library) are each
-- valid alone and collide when both are applied, and a swap of two entries
-- cannot be applied one row at a time at all. So the index stays, for the
-- same lookups and ordering, but no longer refuses a duplicate.
--
-- What the index promised is now the engine's: entries are read in
-- `(position, added_at, book_id)` order, so duplicate or gapped positions
-- still read in one stable order, and every write that changes a list leaves
-- its positions exactly 0..n-1 again.
DROP INDEX idx_book_list_entries_pos;
CREATE INDEX idx_book_list_entries_pos ON book_list_entries(list_id, position);
