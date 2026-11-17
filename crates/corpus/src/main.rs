//! Independent of `bookmole`, so engine bugs cannot leak into the fixtures.

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

// The shared `Gen` prefix is the CLI's vocabulary; renaming the variants would rename the commands.
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
    GenGoodreads {
        /// PRNG seed; the same seed always yields the same file.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// How many rows.
        #[arg(long, default_value_t = 40)]
        books: usize,
        /// Output dir; defaults to the engine's `tests/fixtures/goodreads/generated`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write the committed KOReader statistics fixture as SQL text plus an expected-totals JSON.
    GenKostats {
        /// PRNG seed; the same seed always yields the same file.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Output dir; defaults to the engine's `tests/fixtures/koreader/statistics`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write the seeded dev library: `seed.sql`, covers, a vault, a manifest. Data only, no schema.
    GenDevdb {
        /// PRNG seed; the same seed always yields the same library.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// How many ordinary books behind the deliberate edge cases.
        #[arg(long, default_value_t = 200)]
        books: usize,
        /// Output dir; defaults to `corpus/generated/devdb` (gitignored).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Where the library will be built, distinct from `--out`: cover paths in the seed are absolute.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Derive tier-2 sidecars from the fetched Project Gutenberg epubs.
    GenCorpus {
        /// PRNG seed; output is a pure function of (seed, generator version, epub bytes).
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
    /// Build a real calibre library from the Gutenberg epubs with `calibredb`.
    /// The lock is semantic, not byte-exact, since calibre mints uuids and timestamps.
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
    /// Write a reader's own input: Goodreads shelves and a `verbs.jsonl` the recipe runner replays.
    GenReader {
        /// PRNG seed; the same seed always yields the same reader.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Output dir; defaults to `corpus/generated/reader` (gitignored).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Pull the offline index of well-known works from Wikidata (CC0) into
    /// `crates/engine/data/wellknown.tsv.gz`. Live and opt-in: never run by a test.
    GenWellknown {
        /// The most works to keep, by notability (sitelinks); a cap, not a goal.
        #[arg(long, default_value_t = 200_000)]
        target: usize,
        /// Keep only this many in the file, cut once every candidate's details are in.
        #[arg(long)]
        keep: Option<usize>,
        /// Works per details request.
        #[arg(long, default_value_t = 400)]
        chunk: usize,
        /// The least notability a work may have to be considered at all.
        #[arg(long, default_value_t = 1)]
        min_sitelinks: u32,
        /// The shortest pause between two requests, in seconds.
        #[arg(long, default_value_t = 2)]
        pause: u64,
        /// Where finished batches are kept between runs; defaults to `target/wellknown-scratch`.
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
    /// Apply a recipe (`corpus/recipes/<name>.toml`) to a data directory through the real
    /// `bookmole` binary, then write `build.json`.
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

#[expect(clippy::too_many_lines)]
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
