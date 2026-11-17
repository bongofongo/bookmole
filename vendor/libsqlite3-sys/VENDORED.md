# Vendored `libsqlite3-sys` 0.30.1 with SQLite 3.54.1

**Why.** RUSTSEC-2026-0459 / CVE-2026-119375: SQLite before 3.54.0 can return
uninitialised page-cache memory when a database is opened together with a
`-wal` whose page size differs from the database's. bookmole does open such
pairs from outside — `crates/engine/src/ko_statistics.rs` copies a reader's
`statistics.sqlite3` with its `-wal`/`-shm` and opens them — so the advisory is
real here, not only a scanner finding. The fix upstream is `libsqlite3-sys`
0.39 (SQLite 3.54.x), but `sqlx-sqlite` 0.8.6 needs 0.30.x and 0.9.0 caps at
`<0.38`, so no `sqlx` release can take it.

**What this is.** The crates.io source of `libsqlite3-sys` 0.30.1, unchanged
except:

- `sqlite3/sqlite3.c`, `sqlite3.h`, `sqlite3ext.h` are SQLite **3.54.1**'s
  amalgamation (`https://sqlite.org/2026/sqlite-amalgamation-3540100.zip`,
  SHA3-256 `92281c86e2957f9b9305890f9ac04204caa7b677cc868dc326d942689df28e30`,
  checked against sqlite.org's download page on 2026-11-10);
- `sqlite3/bindgen_bundled_version.rs`: `SQLITE_VERSION`,
  `SQLITE_VERSION_NUMBER` and `SQLITE_SOURCE_ID` set to 3.54.1's. The rest of
  the bindings are 3.46's, which is safe: SQLite's C API only ever adds, so
  3.54 exports every symbol and layout 3.46's bindings declare;
- the `sqlcipher/` tree and its upgrade script are dropped (no build here
  enables SQLCipher).

It is wired in by `[patch.crates-io]` in the workspace `Cargo.toml`.

**Delete it** (and the patch) the day a `sqlx` release accepts
`libsqlite3-sys >= 0.39`. To refresh SQLite before then: replace the three
files from a newer amalgamation, verify its SHA3-256 against sqlite.org, and
update the three constants.
