-- Other annotations of a highlight, kept beside the one it shows.
--
-- `highlights.annotation` (0001) is the reader's own text on a passage. When
-- two rows turn out to be one highlight and are folded together, both may
-- carry an annotation the reader wrote. Neither is a mistake and neither may
-- be lost, so the one the surviving row does not show is set aside here as an
-- alternative. The reader can keep both, make an alternative the annotation
-- (the shown one then moves here), or remove any of them.
--
-- The same text is never held twice for one highlight. An alternative goes
-- with its highlight by the cascade.
--
-- Nothing to back-fill: until now a fold kept only the surviving row's text.
CREATE TABLE annotation_alternatives (
    highlight_id INTEGER NOT NULL REFERENCES highlights(id) ON DELETE CASCADE,
    text         TEXT NOT NULL,
    set_aside_at INTEGER NOT NULL,           -- unix seconds
    PRIMARY KEY (highlight_id, text)
);
