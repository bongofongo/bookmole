# `crates/engine/data/` — the offline index of well-known works

What a search box suggests as you type, what a misspelt search is corrected
against, and the notability half of a result's standing. It is an index rather
than a provider: it answers offline, instantly, and the same way every time,
so suggestions and corrections never wait on the network or change with a
provider's mood.

| file | what it is |
|---|---|
| `wellknown.tsv.gz` | the works: one gzip of UTF-8 lines, sorted by Wikidata id. `gzcat wellknown.tsv.gz \| less` reads it; two versions diff with `diff <(gzcat a) <(gzcat b)`. The layout is described at the top of `crates/corpus/src/wellknown.rs` and read by `crates/engine/src/search/wellknown.rs`. |
| `wellknown.json` | its sha256, build date, work count and size. The engine names its derived database after the sha256, and `tests/wellknown.rs` fails if the two disagree. |

## Where it comes from

**Wikidata**, pulled by `make wellknown` (`cargo run -p corpus --
gen-wellknown`): generic SPARQL against the public query service, never
anything from a library, `User-Agent: bookmole/<version>`, one request at
a time with `Retry-After` honoured. Works are chosen by notability (Wikidata
sitelinks): every direct instance of a class of written work with two or more
sitelinks, then those with one — when that one is a Wikipedia article rather
than a Wikisource page — up to the target. Never edited by hand: to change it,
pull again and commit the new file **in a commit of its own**.

It is data **about the world, not about you**. Nothing in it is derived from a
library, and the engine never writes to it or to the database it derives.

## Licence and attribution

Wikidata's structured data is released under **CC0 1.0** (public domain
dedication, <https://creativecommons.org/publicdomain/zero/1.0/>); no
attribution is required. It is given anyway: *data from Wikidata,
<https://www.wikidata.org>*. The Open Library work ids in it are Wikidata's
statements (`P648`), not a pull from Open Library, whose terms forbid bulk
downloads through its API.

## Built with the engine

The `wellknown` cargo feature (on by default) embeds the gzip. Off, the
engine still builds and runs: suggestions come from the library alone and a
search reports `OfflineIndexUnavailable`.

On first use the engine builds `database/wellknown-<stamp>.db` beside
`app.db` (a trigram fts5 index and its tables; `<stamp>` is the first 16 hex
of the sha256 and the layout version, `INDEX_LAYOUT`). It is derived, never migrated, and safe to delete: the next
search builds it again. An in-memory database has no index unless a test
gives it one.
