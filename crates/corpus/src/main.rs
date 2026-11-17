//! Fixture generator for the KOReader import harness.
//!
//! Two tiers, and the split is deliberate:
//!
//! * **tier 1, `gen-synthetic`** — small, committed, covers every *shape*
//!   (hostile input, the sibling-epub ISBN match path). Runs offline in CI on
//!   every PR. Shape coverage must not sit behind a download, or the branch is
//!   untested on every machine that has not run the fetch script.
//! * **tier 2, `gen-corpus`** — derived from real Project Gutenberg epubs,
//!   gitignored, covers *scale and realism*. Nightly only.
//!
//! This crate does **not** depend on `bookmole`. Reusing the engine's own
//! parsing or normalization to build its fixtures would bake any bug in those
//! straight into the goldens; the generator stays an independent oracle.

mod calibre_lib;
mod devdb;
mod epub_read;
mod goodreads;
mod gutenberg;
mod kindle;
mod kostats;
mod prose;
mod reader;
mod recipe;
mod synthetic;
mod wellknown;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "corpus", about = "Generate KOReader import fixtures")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// The shared `Gen` prefix is the point, not an accident: `gen-synthetic`,
// `gen-goodreads` and `gen-corpus` are the CLI's own vocabulary for "write
// fixtures", and renaming the variants to please the lint would rename the
// commands people type.
#[allow(clippy::enum_variant_names)]
#[derive(Subcommand)]
enum Cmd {
    /// Write the committed tier-1 hostile fixtures.
    GenSynthetic {
        /// Fixture root; defaults to the engine's `tests/fixtures/koreader`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Kindle fixture root; defaults to the engine's `tests/fixtures/kindle`.
        #[arg(long)]
        kindle_out: Option<PathBuf>,
    },
    /// Write the committed Goodreads export fixture.
    ///
    /// Beside the hand-authored files in `.../goodreads/recorded/`, which are a
    /// recording of Goodreads' own dialect and are the documented exception to
    /// "fixtures are generated". This one covers volume and variety instead.
    GenGoodreads {
        /// PRNG seed. ChaCha8, so the same seed is the same file for ever.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// How many rows.
        #[arg(long, default_value_t = 40)]
        books: usize,
        /// Output dir; defaults to the engine's
        /// `tests/fixtures/goodreads/generated`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write the committed KOReader `statistics.sqlite3` fixture.
    ///
    /// Emits **SQL text plus an expected-totals JSON**, not a database file:
    /// see `kostats.rs` for why, and note that the totals are accumulated while
    /// the rows are written rather than read back, which is what makes them an
    /// oracle rather than a recording.
    GenKostats {
        /// PRNG seed. ChaCha8, so the same seed is the same file for ever.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Output dir; defaults to the engine's
        /// `tests/fixtures/koreader/statistics`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write the seeded dev library: `seed.sql`, covers, a vault, a manifest.
    ///
    /// Emits **data, never a schema**. `make dev-db` has the real binary create
    /// and migrate an empty database first, because the schema belongs to the
    /// engine and its `_sqlx_migrations` ledger belongs to sqlx — a second copy
    /// of either here would be free to drift. See `devdb.rs`.
    GenDevdb {
        /// PRNG seed. ChaCha8, so the same seed is the same library for ever.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// How many ordinary books behind the deliberate edge cases.
        #[arg(long, default_value_t = 200)]
        books: usize,
        /// Output dir; defaults to `corpus/generated/devdb` (gitignored).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Where the library will be *built* — `make dev-db`'s `DEV_DB_DIR`.
        ///
        /// Not the same as `--out`, and it has to be stated: `cover_path` is a
        /// whole path in this schema, so the seed cannot name a cover file
        /// without knowing where the database is going to live.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Derive tier-2 sidecars from the fetched Project Gutenberg epubs.
    GenCorpus {
        /// PRNG seed. Output is a pure function of (seed, generator version,
        /// epub bytes), so the same seed always reproduces the same corpus.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Highlights per sidecar.
        #[arg(long, default_value_t = 40)]
        per_book: usize,
        #[arg(long)]
        manifest: Option<PathBuf>,
        #[arg(long)]
        epubs: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Build a real calibre library from the fetched Gutenberg epubs, with
    /// `calibredb` itself, so its metadata warts are the tool's and not ours.
    ///
    /// Semantic lock, not byte lock: calibre mints uuids and timestamps no seed
    /// controls, so `calibre.lock.json` records what the engine must *see*.
    /// See `calibre_lib.rs`.
    GenCalibre {
        /// PRNG seed for which rows get which salted warts.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        manifest: Option<PathBuf>,
        #[arg(long)]
        epubs: Option<PathBuf>,
        /// Output dir; defaults to `corpus/generated/calibre` (gitignored).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write a reader's own input: the "physical reader" Goodreads shelves and
    /// a `verbs.jsonl` of `note`/`cite`/`progress`/`reread`/`set` commands the
    /// recipe runner replays through the CLI. See `reader.rs`.
    GenReader {
        /// PRNG seed. ChaCha8, so the same seed is the same reader for ever.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Output dir; defaults to `corpus/generated/reader` (gitignored).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Pull the offline index of well-known works from Wikidata (CC0) and
    /// write `crates/engine/data/wellknown.tsv.gz` beside its sha256 and build
    /// date.
    ///
    /// **Live, and the only generator that is.** Opt-in, never run by a test:
    /// generic SPARQL against `query.wikidata.org`, one request at a time,
    /// `Retry-After` honoured, resumable from `--scratch`. See `wellknown.rs`.
    GenWellknown {
        /// The most works to keep, by notability (sitelinks). A cap, not a
        /// goal: on 2026-11-12 everything that qualified came to about
        /// 160,000, under it.
        #[arg(long, default_value_t = 200_000)]
        target: usize,
        /// Keep only this many in the file, cut once every candidate's details
        /// are in (notability, then Open Library id, author and year).
        #[arg(long)]
        keep: Option<usize>,
        /// Works per details request.
        #[arg(long, default_value_t = 400)]
        chunk: usize,
        /// The least notability a work may have to be considered at all. At 1,
        /// a work with one sitelink is kept only when that one is a Wikipedia
        /// article (not a Wikisource page).
        #[arg(long, default_value_t = 1)]
        min_sitelinks: u32,
        /// The shortest pause between two requests, in seconds.
        #[arg(long, default_value_t = 2)]
        pause: u64,
        /// Where finished batches are kept between runs; defaults to
        /// `target/wellknown-scratch` (ignored with the build).
        #[arg(long)]
        scratch: Option<PathBuf>,
        /// Output dir; defaults to `crates/engine/data`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Write the file from the scratch directory and ask nothing.
        #[arg(long, default_value_t = false)]
        assemble_only: bool,
        /// The build date to stamp (`YYYY-MM-DD`); defaults to today, UTC.
        #[arg(long)]
        built: Option<String>,
    },
    /// Apply a recipe (`corpus/recipes/<name>.toml`) to a data directory
    /// through the real `bookmole` binary, then write `build.json`.
    /// See `recipe.rs` and `corpus/recipes/README.md`.
    BuildRecipe {
        /// A recipe name under `corpus/recipes/`, or a path to a `.toml`.
        recipe: String,
        /// The library to build. Created if absent; must be absolute or is made so.
        #[arg(long)]
        data_dir: PathBuf,
        /// The `bookmole` binary to drive; defaults to the workspace's debug build.
        #[arg(long)]
        bin: Option<PathBuf>,
        /// Rewrite `corpus/recipes/expect/<name>.json` from this build instead of comparing.
        #[arg(long, default_value_t = false)]
        update_expect: bool,
    },
}

fn main() -> std::io::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::GenSynthetic { out, kindle_out } => {
            let root = out.unwrap_or_else(default_fixture_root);
            let written = synthetic::generate(&root)?;
            println!("wrote {written} fixture files under {}", root.display());
            let kindle_root = kindle_out.unwrap_or_else(default_kindle_root);
            let written = kindle::generate(&kindle_root)?;
            println!(
                "wrote {written} Kindle fixture files under {}",
                kindle_root.display()
            );
            println!("now run `make golden` to record their expected behaviour");
            Ok(())
        }
        Cmd::GenGoodreads { seed, books, out } => {
            let out = out.unwrap_or_else(goodreads::default_out);
            let n = goodreads::generate(&out, &goodreads::Options { seed, books })?;
            println!(
                "wrote {n} rows (v{}, seed {seed}) to {}/library.csv",
                goodreads::GENERATOR_VERSION,
                out.display()
            );
            Ok(())
        }
        Cmd::GenKostats { seed, out } => {
            let out = out.unwrap_or_else(kostats::default_out);
            let n = kostats::generate(&out, &kostats::Options { seed })?;
            println!(
                "wrote {n} statistics books (v{}, seed {seed}) to {}/statistics.sql",
                kostats::GENERATOR_VERSION,
                out.display()
            );
            Ok(())
        }
        Cmd::GenDevdb {
            seed,
            books,
            out,
            data_dir,
        } => {
            let out = out.unwrap_or_else(devdb::default_out);
            let data_dir = data_dir.unwrap_or_else(devdb::default_data_dir);
            let n = devdb::generate(&out, &data_dir, seed, books)?;
            println!(
                "wrote {n} books, {} of them deliberate edge cases (v{}, seed {seed}) to {}/seed.sql",
                devdb::edge_case_count(),
                devdb::GENERATOR_VERSION,
                out.display()
            );
            println!("now run `make dev-db` to build a database from it");
            Ok(())
        }
        Cmd::GenCorpus {
            seed,
            per_book,
            manifest,
            epubs,
            out,
        } => {
            let manifest = manifest.unwrap_or_else(gutenberg::default_manifest);
            let epubs = epubs.unwrap_or_else(gutenberg::default_epub_dir);
            let out = out.unwrap_or_else(gutenberg::default_out);
            let n = gutenberg::generate(
                &manifest,
                &epubs,
                &out,
                &gutenberg::Options { seed, per_book },
            )?;
            println!(
                "wrote {n} sidecars (v{}, seed {seed}) under {}",
                gutenberg::GENERATOR_VERSION,
                out.display()
            );
            Ok(())
        }
        Cmd::GenCalibre {
            seed,
            manifest,
            epubs,
            out,
        } => {
            let manifest = manifest.unwrap_or_else(gutenberg::default_manifest);
            let epubs = epubs.unwrap_or_else(gutenberg::default_epub_dir);
            let out = out.unwrap_or_else(calibre_lib::default_out);
            let n = calibre_lib::generate(&manifest, &epubs, &out, &calibre_lib::Options { seed })?;
            println!(
                "wrote a calibre library of {n} books (v{}, seed {seed}) under {}",
                calibre_lib::GENERATOR_VERSION,
                out.display()
            );
            Ok(())
        }
        Cmd::GenReader {
            seed,
            manifest,
            out,
        } => {
            let manifest = manifest.unwrap_or_else(gutenberg::default_manifest);
            let out = out.unwrap_or_else(reader::default_out);
            let n = reader::generate(&manifest, &out, &reader::Options { seed })?;
            println!(
                "wrote {n} reader verbs (v{}, seed {seed}) under {}",
                reader::GENERATOR_VERSION,
                out.display()
            );
            Ok(())
        }
        Cmd::GenWellknown {
            target,
            keep,
            chunk,
            min_sitelinks,
            pause,
            scratch,
            out,
            assemble_only,
            built,
        } => {
            let report = wellknown::generate(&wellknown::Options {
                scratch: scratch.unwrap_or_else(wellknown::default_scratch),
                out: out.unwrap_or_else(wellknown::default_out),
                target,
                keep,
                chunk,
                min_sitelinks,
                min_pause: std::time::Duration::from_secs(pause),
                assemble_only,
                built,
            })
            .map_err(std::io::Error::other)?;
            println!(
                "wrote {} works ({} bytes, sha256 {}) to {}",
                report.works,
                report.bytes,
                report.sha256,
                report.path.display()
            );
            Ok(())
        }
        Cmd::BuildRecipe {
            recipe,
            data_dir,
            bin,
            update_expect,
        } => {
            let report = recipe::build(
                &recipe,
                &data_dir,
                bin.as_deref(),
                &recipe::Options { update_expect },
            )?;
            println!("{report}");
            Ok(())
        }
    }
}

fn default_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/koreader")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("crates/engine/tests/fixtures/koreader"))
}

fn default_kindle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/fixtures/kindle")
}
