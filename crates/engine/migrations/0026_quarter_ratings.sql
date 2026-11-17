-- Ratings in quarters, on both scales.
--
-- Goodreads now takes quarter ratings, so `My Rating` is no longer an integer
-- 0–5 and the two seeded scales can mean the same thing: `default` and
-- `goodreads` both become 0 to 5 in steps of 0.25, and the map between them is
-- the identity at every point. So the two map one-to-one, with
-- **no rounding rule** — which is what `0007` refused to write for the halves,
-- and the reason is gone rather than overruled: a half star now *has* an
-- honest Goodreads value, and it is itself.
--
-- ## The map's column becomes REAL, and that needs a rebuild
--
-- `rating_map.goodreads` was `INTEGER`, the right type for a value that was one.
-- SQLite would store 4.25 in it anyway (INTEGER affinity keeps a REAL it cannot
-- convert losslessly) but would hand 4.0 back as the integer 4, so one column
-- would carry two storage classes depending on the value — the sort of thing a
-- typed read trips over one row in four. A column cannot change type in place,
-- so the table is rebuilt: copy, drop, rename. Nothing references `rating_map`
-- (`review_ratings` points at `rating_scales`, not at the map), so the drop
-- cascades nowhere. The CHECK is Goodreads' own range; the 0.25 grid is checked
-- where the engine quantizes, as every other point on every scale is.
--
-- ## Which scales move
--
-- Only the two by these names, and only while they still have the seeded range
-- (0 to 5) and a step the quarter grid contains (0.5, 1, or 0.25 already). That
-- condition is what keeps every existing rating valid: each value on a grid of
-- 0.5 or 1 from 0 is on the grid of 0.25 from 0, so no `review_ratings` row
-- moves off its scale. A `default` the user redefined (`rating scale default
-- 0 10 1`) is theirs and is left exactly as it is, map included.
--
-- ## The map is filled, never rewritten
--
-- Every quarter point gets an identity row **only where it has none**
-- (`INSERT OR IGNORE`). The seeded whole stars were already the identity, so
-- they are unchanged; a point the user mapped by hand (`rating map 4.5 5`, the
-- only way a half star could reach Goodreads before) keeps their answer. The
-- map is user-editable and a migration that overwrote an edit would be deciding
-- for them — `rating map 4.5 4.5` is the one-line way back to the identity.
CREATE TABLE rating_map_quarters (
    scale_id  INTEGER NOT NULL REFERENCES rating_scales(id) ON DELETE CASCADE,
    value     REAL NOT NULL,
    goodreads REAL NOT NULL CHECK (goodreads >= 0 AND goodreads <= 5),  -- 0 = unrated
    PRIMARY KEY (scale_id, value)
);
INSERT INTO rating_map_quarters (scale_id, value, goodreads)
SELECT scale_id, value, CAST(goodreads AS REAL) FROM rating_map;
DROP TABLE rating_map;
ALTER TABLE rating_map_quarters RENAME TO rating_map;

UPDATE rating_scales
   SET step = 0.25
 WHERE name IN ('default', 'goodreads')
   AND min = 0.0 AND max = 5.0
   AND step IN (0.25, 0.5, 1.0);

-- Every point 0, 0.25 … 5 — exact in binary, so the sum never drifts off the
-- key `RatingScale::value_at` computes for the same point.
WITH RECURSIVE q(v) AS (SELECT 0.0 UNION ALL SELECT v + 0.25 FROM q WHERE v < 5.0)
INSERT OR IGNORE INTO rating_map (scale_id, value, goodreads)
SELECT s.id, q.v, q.v
  FROM rating_scales s, q
 WHERE s.name IN ('default', 'goodreads')
   AND s.min = 0.0 AND s.max = 5.0 AND s.step = 0.25;
