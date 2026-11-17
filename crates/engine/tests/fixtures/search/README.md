# Search fixtures

## `bench/` — the search benchmark

`tests/search_bench.rs` runs every query in `bench/queries.json` through
`bench_search` (`Engine::search`, called as a frontend's add-a-book box calls
it; for *you already have it*, a search session run to its end with each
work's `Held` as the held answer), scores
where the right book landed, prints the table and fails if a gated figure
drops below `bench/baseline.json`.

| file | what it is |
|---|---|
| `queries.json` | ~190 public, famous-book queries written for the benchmark, each with a category and the book it wants as a set of acceptable identities (`ol_work`, `wikidata`, `isbns`, and `title` + `author` compared folded). `library` is the small public library the *you already have it* category is asked against, seeded by the test, each book with a copy so it is in the library. Ten of that category's queries carry `"scope": "library"`: a book of the reader's own typed loosely, asked of the library's own search (`Engine::search_library`), which is local and needs no recording. Nothing here comes from anyone's real library. |
| `baseline.json` | each gated category's three figures (right book at 1, in the top five, MRR), plus `overall`. |
| `recorded/<provider>/` | what each provider sent: one body file per request, verbatim, and an `index.json` mapping the request URL (any API key scrubbed to `key=REDACTED`) to the file and the status it came with. |

### Recordings are never edited by hand

A recording is a response from another system, kept so the benchmark can run
offline with the real provider code doing the URL building and the parsing. It
is evidence of what that system says, and the same rule as
`fixtures/goodreads/README.md` applies: a body written or trimmed by hand — or
from memory — would prove only that we agree with ourselves. So:

- never edit, reformat or trim a body file; size is kept down by asking for
  modest limits in the request, never by cutting a response;
- never add an `index.json` entry by hand;
- to change a recording, re-record it.

### Recording

```
make search-bench-record          # fetch what is missing
make search-bench-record ALL=1    # fetch everything again
```

This is the only path that sends a benchmark query anywhere. It runs each query
through the real provider code against a loopback server; whatever the server
has no recording for is fetched from the live host at one request a second with
`User-Agent: bookmole/<version>` and nothing else, written here, and the
query is run again until it needs nothing new. At the end it drops recordings
no query asked for, and checks every expected identity against live Open
Library: each `ol_work` must exist and be by the expected author, each ISBN must
be an edition of its work. A 429 or an unreachable host stops the run; nothing
is made up to fill the gap.

- **Open Library** is always recorded.
- **Google Books** is recorded only when `GOOGLE_BOOKS_API_KEY` is set (keyless
  calls get 429 on every request). Without its recordings the benchmark leaves
  Google out and the table says *not recorded*; that is not a failure.
- **Changing a provider's request** (a new `fields` list, a new parameter)
  changes its URLs, so its old recordings stop matching: re-record in the same
  change, and the diff shows what the provider now answers.

Budget: 15 MB for everything under `recorded/`.

### Replaying

`make search-bench` (or the ordinary test suite, silently). A request with no
recording is answered with a 501 and fails its query by URL; the run never
reaches the network. `UPDATE_BASELINE=1` rewrites `baseline.json`; a change
that moves a figure commits the rewrite in the same change, and a change that
lowers a gated one is not finished. `SEARCH_BENCH_VERBOSE=1` prints one line per query.

### The offline index

The run opens the **real** embedded offline index (`crates/engine/data/`),
built once into `target/tmp/wellknown` and reopened by later runs, so the
first run after a new data file takes a few seconds longer. A corrected query
asks Open Library for its fix as `title=…&author=…`: a URL the uncorrected
query never asks for, recorded like any other by `make search-bench-record` (the fix
comes from Wikidata, so the request carries nothing but a famous book's title
and author). Two lines follow the table and are reported, not gated:

- **as typed** — the typos, translations and series queries through
  `Engine::search_as_typed`, never corrected: the figures correction is argued
  against.
- **corrections** — how many queries a session corrected, on typos and
  elsewhere. A correction outside *typos* is a query that already found its
  book, and **fails the run** by id.
