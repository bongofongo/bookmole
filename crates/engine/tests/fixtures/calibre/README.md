# Calibre fixtures

## `recorded/library.json`

Test fixtures in this repository are generated, not hand-written (see
`crates/corpus`). This file is the documented exception, shared with the
Goodreads `recorded/` files and for the same reason: **`calibredb list
--for-machine` output is a recorded artifact of another system.** Writing one
from our own understanding of the format would prove only that we agree with
ourselves.

It was recorded from **calibre 9.11** (`calibredb --with-library … list
--for-machine --fields all`) against a throwaway library built from two
Project Gutenberg epubs in `corpus/epub/` (2591, *Grimms' Fairy Tales*, and
1952, *The Yellow Wallpaper*) plus one `calibredb add --empty` book:

```sh
calibredb --with-library "$LIB" add corpus/epub/2591.epub corpus/epub/1952.epub
calibredb --with-library "$LIB" add --empty --title "Untitled Draft"
ebook-convert 2591.epub 2591.azw3 && calibredb --with-library "$LIB" add_format 1 2591.azw3
calibredb --with-library "$LIB" set_metadata 1 \
  --field identifiers:isbn:9780140447101,goodreads:22917 \
  --field "publisher:Penguin Classics" --field rating:5 \
  --field "series:Household Tales" --field series_index:1 \
  --field tags:fairy-tales,german-lit --field "comments:<p>…</p>"
calibredb --with-library "$LIB" set_metadata 2 --field pubdate: --field tags:
calibredb --with-library "$LIB" list --for-machine --fields all
```

`set_metadata` is there to reach the shapes a freshly-added Gutenberg book
does not have; the ISBN, Goodreads id, publisher and series are illustrative
values typed in, not facts about a particular edition. Three edits were made
by hand afterwards: the library's absolute path was replaced with
`/books/calibre` (the tests root it in a temp dir again); row 1's `pubdate`
was moved from `2001-03-31T23:00:00+00:00` to `2001-04-01T00:00:00+00:00`,
because calibre renders a typed date through the recording machine's local
timezone and the hour said which one; and row 3 was cut down as described
below. Every other key, type and sentinel is exactly what calibre wrote.

The shapes, and why each one is here:

| shape | why it is here |
|---|---|
| `"authors": "Jacob Grimm & Wilhelm Grimm"` — a **`&`-joined string** | Every other multi-valued field (`tags`, `languages`, `formats`) comes back as a JSON array. Deserializing `authors` as one is the obvious guess and it fails on the first book, at runtime, in a `serde` error nobody can act on. |
| `"pubdate": "0101-01-01T00:00:00+00:00"` on *The Yellow Wallpaper* | Calibre's `UNDEFINED_DATE` sentinel, which is what clearing a book's publication date leaves behind. Taken at face value it gives every undated book a `publish_year` of **101**: green tests, wrong shelf. |
| `"series_index": 1.0` on every row, including the two with no `series` | Calibre's default, not a claim that a standalone book is the first of something. The index is read only where a series name is. |
| `"isbn": ""` beside a populated `identifiers` map | The dedicated column is empty far more often than not, and the map holds other schemes too (`uri` here, which Gutenberg's own metadata supplies). On row 1 the ISBN appears in both places; the map's `isbn` key holds ASINs and typos as readily as ISBNs, so it goes through `normalize_isbn` like everything else. |
| `"rating": 10` | `list` reports 0–10 half-stars; `set_metadata --field rating:5` is what wrote it. |
| two `formats`, listed in name order (`.azw3` before `.epub`) | Every format is recorded and hashed, not just the epub. |
| row 3, carrying almost nothing | Calibre **omits** a key entirely when the field has no value rather than writing null: row 2 simply has no `rating`, `series`, `publisher` or `comments` key, and an empty book as recorded has no `cover` or `size` either. A struct without `#[serde(default)]` on every field refuses such a row. Row 3 takes that to its limit and is the one row reduced by hand: it keeps only `id`, `series_index`, `timestamp`, `title` and `uuid`, so that a row missing *every* optional key is pinned. As calibre 9.11 actually writes it, the row also carries `"authors": "Unknown"`, `"author_sort": ""`, `"isbn": ""`, `[]`/`{}` for the empty lists and the undated `pubdate` sentinel. `"Unknown"` is calibre's default author, a placeholder for none, and `calibre.rs` reads it as no author (`calibres_unknown_author_is_no_author`). |

Two further facts about calibre have no fixture because they are not about
parsing, and both are asserted in `calibre.rs` instead:

- `calibredb --with-library /a/typo list` **creates a library there** —
  `metadata.db`, `.calnotes/` — and reports `[]` with exit 0 (observed on
  calibre 7.26 and again on 9.11). A mistyped path is otherwise
  indistinguishable from an empty library, having scribbled on the user's disk
  on the way past. `library_root` refuses a directory with no `metadata.db`
  before the binary runs.
- `rating` is 0–10 half-stars in `list` output while `set_metadata --field
  rating:N` takes 0–5. It is not imported at all: a rating here lives on a
  *review*, which anchors to a *reading*, which calibre knows nothing about.

There is no `generated/` tier. The Goodreads generator earns its place by
covering volume and variety in a format with real quoting hazards; a JSON array
of the same three shapes repeated four hundred times would test `serde_json`.
