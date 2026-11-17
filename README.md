# bookmole

A local-first reading companion: your library, your highlights and your notes,
kept in one place on your own machine — with a terminal reading room whose
centrepiece is a ray-traced 3D book drawn in Unicode block glyphs.

It reads what you already have rather than asking you to re-enter it:

- **KOReader** highlights, notes and reading state, off a plugged-in reader or
  over the local network, plus the reader's own reading-time statistics;
- **Kindle** clippings and **Apple Books** highlights;
- a **calibre** library, and **Goodreads** CSV exports (in and out);
- **EPUB** and **PDF** files, read for their embedded metadata.

Book metadata comes from a federated search of OpenLibrary and Google Books.
Notes are plain Markdown in a vault folder you can also open in Obsidian, with
links between notes and citations of highlights; single-word highlights can be
captured as flashcards. Everything is stored in a local SQLite database.
Where data has an origin elsewhere — KOReader owns highlights and reading
state, calibre owns files, the providers own bibliographic metadata —
bookmole keeps a durable local copy, records where each field came from,
and resolves conflicts toward that origin.

## What is in this repository

| crate | binary | what it is |
|---|---|---|
| `crates/engine` | — | the library (package `bookmole`): storage, imports, search, the vault |
| `crates/cli` | `bookmole` | the command line; `bookmole --help` for the tree, `bookmole repl` for an interactive prompt |
| `crates/tui` | `bookmole-tui` | the terminal reading room, designed to live in a tmux pane |
| `crates/api` | — | the versioned API: the engine's surface as serializable requests and responses |
| `crates/daemon` | `bookmoled` | serves that API as newline-delimited JSON over a unix socket |
| `crates/book3d` | — | the ray-traced book, as RGBA pixels for any frontend |
| `crates/epubkit` | — | a standalone EPUB reader (container, OPF, navigation), no bookmole types |
| `crates/corpus` | `corpus` | generates the test fixtures and the testing corpus; never shipped |

`fuzz/` holds the cargo-fuzz targets and their committed seeds. A separate
desktop application builds on these crates through `bookmole-api`; it is
not part of this repository.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/bongofongo/bookmole/main/install.sh | sh
```

Prebuilt for x86_64 and arm64 on both Linux (glibc 2.35+) and macOS (11.0+).
The script downloads the release archive for your machine, checks it against the
release's `SHA256SUMS`, and puts two binaries in `~/.local/bin`:

| | |
|---|---|
| `bookmole-tui` | the reading room — run it in a tmux pane |
| `bookmole` | the command line — `bookmole --help` |

It edits no shell profile and installs nothing system-wide. Knobs, all
environment variables:

```sh
RB_VERSION=v0.1.0          # pin a version (default: the latest release)
RB_INSTALL_DIR=~/bin       # where the binaries go (default: ~/.local/bin)
RB_NO_VERIFY=1             # skip the checksum (default: verify, and refuse on mismatch)
```

Prefer to read it first? It is one file: [`install.sh`](install.sh). The
release archives themselves are on the repository's Releases page.

### Where your library lives

One place, whichever directory you start from, and every frontend opens the
same one:

- macOS: `~/Library/Application Support/bookmole`
- Linux: `$XDG_DATA_HOME/bookmole`, or `~/.local/share/bookmole` when
  that is unset

Inside it: `database/app.db` (created and migrated on first start),
`database/images/` for covers, `database/files/` for the ebook files you keep
here, and `vault/` for your notes.

To keep it somewhere else, set `BOOKMOLE_DATA_DIR` in your shell profile
(`--data-dir <path>` does the same for one command):

```sh
export BOOKMOLE_DATA_DIR="$HOME/reading"
```

## Using it

In the TUI, `m` opens the menu from anywhere, and the key bar at the bottom of
a screen is always the answer to "what can I press". `bookmole-tui --book
<selector>` opens straight into a book (an id, an ISBN or a title fragment).

On the command line, `bookmole search <query>` finds a book and
`bookmole add <isbn>` adds it, `bookmole epub <file>` imports one you
own, `bookmole ko sync` pulls from a plugged-in KOReader device, and
`bookmole --help` lists everything else.

The daemon is for programs rather than people:

```sh
bookmoled --data-dir "$HOME/reading"   # listens on <data-dir>/bookmoled.sock
printf '{"id":1,"request":{"method":"list_books","params":{"limit":5}}}\n' \
  | nc -U "$HOME/reading/bookmoled.sock"
```

One JSON object per line in each direction, each reply carrying the `id` of
the request it answers.

## Building from source

```sh
git clone https://github.com/bongofongo/bookmole
cd bookmole
cargo build --release -p bookmole-cli -p bookmole-tui -p bookmoled
```

The toolchain is pinned in `rust-toolchain.toml`, which rustup picks up on the
first `cargo` call. You need a C compiler: Lua and SQLite are vendored and
built from source, so there are no system libraries to install.

When you run a development build, give it a data directory of its own
(`--data-dir`, or `BOOKMOLE_DATA_DIR`) unless you mean it to open your real
library. `make dev-db` (which needs the `sqlite3` command-line tool) builds a
seeded library of about two hundred books at `dev-data/` that is worth
rendering:

```sh
make dev-db
cargo run -p bookmole-tui -- --data-dir "$PWD/dev-data"
```

## Tests

Everything runs offline; no test touches the network.

```sh
make help        # every target, one line each
make test        # the whole workspace
make test-fast   # the engine only, property tests cut to 32 cases: the inner loop
make ci          # what CI gates on: fmt, clippy, the internals-off build, cargo-deny, tests
```

`cargo-nextest`, `cargo-deny`, `cargo-llvm-cov`, `cargo-mutants` and
`cargo-fuzz` are all optional: a target that wants one prints `SKIPPED:` when it
is not installed rather than failing. `make skips` lists the tests that passed
only because a fixture they want is absent, and
`BOOKMOLE_REQUIRE_FIXTURES=1 make test` turns those skips into failures.

## Licence

[MPL-2.0](LICENSE), except `crates/epubkit`, which is
[MIT](crates/epubkit/LICENSE-MIT) OR [Apache-2.0](crates/epubkit/LICENSE-APACHE)
so that any EPUB tool can borrow it. MPL-2.0 is copyleft per file: you may
build anything on the engine, and changes to its own files stay open.
