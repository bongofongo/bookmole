# bookmole — repeatable test/lint entry points.
#
# Wraps cargo + cargo-nextest. nextest is auto-detected; if it is not
# installed every target degrades to plain `cargo test` so nothing hard-requires
# it. Install for faster parallel runs: `cargo install cargo-nextest`.

# Detect nextest once.
NEXTEST := $(shell command -v cargo-nextest 2>/dev/null)
ifdef NEXTEST
  RUN = cargo nextest run
  RUN_FILTER = cargo nextest run -E
else
  RUN = cargo test
  RUN_FILTER = cargo test --test
endif

# Detected the same way, the same reason: a target reaching for one of these
# must degrade to a stated `SKIPPED:` rather than an opaque "command not
# found", on a machine that has not installed the tool.
DENY := $(shell command -v cargo-deny 2>/dev/null)
LLVM_COV := $(shell command -v cargo-llvm-cov 2>/dev/null)
MUTANTS := $(shell command -v cargo-mutants 2>/dev/null)
CARGO_FUZZ := $(shell command -v cargo-fuzz 2>/dev/null)

.PHONY: help test test-engine test-import test-fast search-bench search-bench-record wellknown skips golden corpus corpus-check synthetic goodreads kostats lint build-check fmt fmt-check deny migrations-check nightly coverage coverage-html mutants mutants-diff fuzz check ci clean dist bench bench-box bench-trend perf dev-db corpus-layers corpus-build corpus-verify corpus-all

# Perf output, kept so runs can be compared over time.
#
# Two shapes, on purpose. Each run gets its own timestamped JSONL + summary
# (they describe one run and would be meaningless concatenated), while
# `history.tsv` accumulates one row per mode per run — that is the trend line,
# and it is what "did this change help" is actually read off.
# Absolute: cargo runs a test binary from the package root, so a relative path
# would put `make perf`'s log under crates/tui/ instead of here.
PERF_DIR ?= $(CURDIR)/perf
PERF_STAMP := $(shell date +%Y%m%d-%H%M%S)
PERF_LOG ?= $(PERF_DIR)/$(PERF_STAMP)-bench.jsonl
PERF_SUMMARY ?= $(PERF_DIR)/$(PERF_STAMP)-bench.txt
PERF_HISTORY ?= $(PERF_DIR)/history.tsv

# Repeats of the bench script, e.g. `make bench BENCH_REPS=3`. Modes interleave
# across reps, so a transient cannot land entirely on one of them. Pass anything
# else through with BENCH_ARGS, e.g. BENCH_ARGS="--book middlemarch".
BENCH_REPS ?= 1
# Seed for the tier-2 corpus. Output is a pure function of (seed, generator
# version, epub bytes), so changing this changes every generated sidecar.
CORPUS_SEED ?= 42
BENCH_ARGS ?=
# The library `make bench` renders from. Explicit because the TUI defaults to
# the platform data directory, not the cwd: without the flag a bench run would
# open the user's real library. BOOKMOLE_DATA_DIR, else the repo root; point
# it at `dev-data` (`make dev-db`) for a library with covers.
BENCH_DATA_DIR ?= $(or $(BOOKMOLE_DATA_DIR),$(CURDIR))

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
	  awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

test: ## Run the whole workspace test suite
	$(RUN) --workspace --locked

test-fast: ## Inner loop: engine tests only, proptest cases cut to 32 (CI keeps 256)
	@# The default 256 cases is what makes the storage proptests (each opening a
	@# fresh in-memory SQLite per case) the critical path of a full `make test`.
	@# 32 is enough to catch a broken property while iterating; CI still runs
	@# the full count, so this is never the thing that decides a mutant is dead.
	PROPTEST_CASES=32 $(RUN) -p bookmole --locked

search-bench: ## The search benchmark's table, replayed offline, checked against baseline.json (UPDATE_BASELINE=1 rewrites it)
	@# Plain `cargo test` with --nocapture, never nextest: the table is the
	@# output, and cargo captures a passing test's stdout. The same test runs in
	@# the ordinary suite as the ratchet, silently.
	UPDATE_BASELINE=$(UPDATE_BASELINE) cargo test -p bookmole --locked --test search_bench -- --nocapture

search-bench-record: ## Record the search benchmark's missing provider responses, live at 1 req/s (ALL=1: all of them)
	@# The only target that sends a benchmark query anywhere. Google Books is
	@# recorded only when GOOGLE_BOOKS_API_KEY is set. See
	@# crates/engine/tests/fixtures/search/README.md.
	BOOKMOLE_SEARCH_RECORD=1 ALL=$(ALL) UPDATE_BASELINE=$(UPDATE_BASELINE) cargo test -p bookmole --locked --test search_bench -- --nocapture

wellknown: ## Pull the offline index of well-known works from Wikidata (live, slow, polite; resumable) into crates/engine/data/
	@# The one generator that talks to the network: generic SPARQL against
	@# query.wikidata.org, one request at a time, Retry-After honoured. Its
	@# scratch (target/wellknown-scratch) makes a re-run resume; delete it to
	@# pull afresh. Then `make search-bench` and commit the data file alone.
	cargo run -p corpus --locked -- gen-wellknown

skips: ## Which tests skipped for want of a fixture — they are silent in `make test`
	@# A fixture-gated test prints `SKIPPED:` and then PASSES, and cargo captures
	@# a passing test's stderr — so the engine standards' "no silently-skipping
	@# tests" rule is carried by a marker that an ordinary run cannot show you.
	@# This target is the reading of it. Three deliberate choices:
	@#
	@# `cargo test`, never the RUN variable. nextest captures per-test and only
	@# replays output for failures, and its --no-capture serialises the entire
	@# run to hand back these same few lines. Plain cargo test gives them up
	@# without the tax, so this is the one target that does not want nextest.
	@#
	@# --no-fail-fast, because a real failure elsewhere must not truncate the
	@# list: the answer to "what did not assert here" should not depend on
	@# whether something else happens to be broken. This target reports and
	@# never gates — `BOOKMOLE_REQUIRE_FIXTURES=1 make test` is the gate,
	@# and scheduled.yml is where it runs.
	@# One marker printed = one test that did not assert, so the RAW line count
	@# is the test count. The list is folded because one absent file commonly
	@# silences several tests, and a bare
	@# `sort -u` would have quietly reported the number of missing FILES under
	@# the word "tests" — six where the answer is eight. Both numbers, then.
	@raw=$$(mktemp); folded=$$(mktemp); \
	cargo test --workspace --no-fail-fast -- --nocapture 2>&1 \
	  | grep -aoE 'SKIPPED[: ].*' > "$$raw" || true; \
	sort "$$raw" | uniq -c | sort -rn > "$$folded"; \
	n=$$(wc -l < "$$raw" | tr -d ' '); \
	m=$$(wc -l < "$$folded" | tr -d ' '); \
	echo ""; \
	if [ "$$n" -gt 0 ]; then \
	  echo "$$n tests passed without asserting anything ($$m fixtures absent):"; \
	  awk '{ c = $$1; $$1 = ""; sub(/^ /, ""); \
	         if (c > 1) printf "  %s  (x%d)\n", $$0, c; \
	         else printf "  %s\n", $$0 }' "$$folded"; \
	  echo ""; \
	  echo "Each wants a fixture that is gitignored by design (your own epubs,"; \
	  echo "your own KOReader exports). To make them failures instead:"; \
	  echo "  BOOKMOLE_REQUIRE_FIXTURES=1 make test"; \
	else \
	  echo "No skips — every test on this machine asserted something."; \
	fi; \
	rm -f "$$raw" "$$folded"

test-import: ## Run only the KOReader import harness
ifdef NEXTEST
	cargo nextest run -p bookmole --locked -E 'test(/koreader_import/)'
else
	cargo test -p bookmole --locked --test koreader_import
endif

golden: ## Regenerate the golden import snapshots and the committed-epub snapshot
	UPDATE_GOLDEN=1 cargo test -p bookmole --test koreader_import import_matches_golden
	UPDATE_GOLDEN=1 cargo test -p bookmole --test epub_corpus every_committed_epub_reads_as_the_golden_says

bench: ## Compare renderers end-to-end (REAL, ACTIVE pane; BENCH_REPS=3 to average)
	@echo "~$$(( ($(BENCH_REPS) * 3 * 265) / 20 ))s, taking over the terminal."
	@echo "It must run in a real pane, and inside tmux that pane must be the"
	@echo "ACTIVE one — tmux routes input to the focused pane only, so a"
	@echo "background pane gets no replies and the latency columns come back empty."
	@echo
	@echo "One rep is enough for the byte columns, which are deterministic."
	@echo "The latency columns are not: they pick up whatever else the machine is"
	@echo "doing, so use BENCH_REPS=3 before believing a surprising one."
	@echo
	@mkdir -p $(PERF_DIR)
	@# The summary is printed only after the terminal is restored, so capturing
	@# stderr and replaying it afterwards loses nothing and keeps a copy.
	cargo run --release -p bookmole-tui -- --data-dir "$(BENCH_DATA_DIR)" \
	  --bench-render all --bench-reps $(BENCH_REPS) $(BENCH_ARGS) \
	  --perf-log $(PERF_LOG) --perf-history $(PERF_HISTORY) \
	  2> $(PERF_SUMMARY); st=$$?; cat $(PERF_SUMMARY); exit $$st
	@echo
	@echo "per-frame log : $(PERF_LOG)"
	@echo "summary       : $(PERF_SUMMARY)"
	@echo "trend         : $(PERF_HISTORY)"

bench-box: ## Same comparison, but in a disposable terminal (no pane hijack)
	@# A throwaway kitty + its own tmux server, so the run needs no terminal of
	@# yours, cannot touch your panes or your library, and comes back with a
	@# pinned window size and font. Rows land in the same history.tsv tagged
	@# env=box — trend those against each other, never against env=live.
	@# Its own reps default (3, because rep 1 warms the fresh terminal) is left
	@# alone unless BENCH_BOX_REPS is set — passing BENCH_REPS's 1 through here
	@# would silently reinstate the cold-terminal reading.
	$(if $(BENCH_BOX_REPS),BENCH_REPS=$(BENCH_BOX_REPS) ,)scripts/bench-sandbox.sh $(BENCH_ARGS)

bench-trend: ## Show every recorded bench run, oldest first
	@test -f $(PERF_HISTORY) || { echo "no history yet — run 'make bench'"; exit 1; }
	@column -t -s "$$(printf '\t')" $(PERF_HISTORY)

perf: ## Cost + wire-rate sweeps, both renderers (release, ignored tests)
	@mkdir -p $(PERF_DIR)
	BOOKMOLE_PERF_LOG=$(PERF_DIR)/$(PERF_STAMP)-sweep.jsonl \
	  cargo test --release -p bookmole-tui --bins -- \
	  --ignored --nocapture --test-threads 1 \
	  glyph_cost raster_cost frame_budget glyph_wire_rate wire_rate

lint: ## Clippy across the workspace (warnings are errors, same as CI)
	cargo clippy --workspace --all-targets -- -D warnings

# NOT subsumed by `lint`, and the difference is load-bearing. `--all-targets`
# resolves dev-dependencies, and `crates/tui`'s dev-dependencies switch on the
# engine's `internals` feature — so under clippy the `Engine::storage()` escape
# hatch exists for every target in the graph, including the shipped binaries.
# This is the build in which it does not, which makes it the only thing standing
# between the closed API seam (frontends reach the engine through
# `bookmole-api`, never `Engine::storage()`) and a frontend quietly
# reopening it.
build-check: ## Build the shipped targets only — the `internals` feature must not be needed
	cargo check --workspace --locked

fmt: ## Format all crates
	cargo fmt --all

fmt-check: ## Verify formatting without writing
	cargo fmt --all --check

deny: ## cargo-deny: bans + licenses + sources — the same scope as CI's `deny` job
ifdef DENY
	cargo deny check bans licenses sources
else
	@echo "SKIPPED: cargo-deny not installed — 'cargo install cargo-deny'."
endif

migrations-check: ## Local twin of CI's migrations job — refuse an applied migration that changed
	@# sqlx records a checksum of every migration it has applied, so editing one
	@# in place breaks every database that already ran it. CI enforces this as
	@# its own PR-only job (it needs the PR's merge base); this is the same
	@# check against origin/main, for before you push. The exclusions, listed in
	@# .github/amended-migrations.txt, are the comment-only amendments that `AMENDED`
	@# (crates/engine/src/storage/mod.rs) repairs and pins.
	@changed=$$(git diff --diff-filter=MDR --name-only \
	  "$$(git merge-base origin/main HEAD)...HEAD" -- 'crates/engine/migrations/*.sql' \
	  $$(grep -v '^#' .github/amended-migrations.txt | sed 's/^/:!/')); \
	if [ -n "$$changed" ]; then \
	  echo "an applied migration was changed, not added:"; \
	  echo "$$changed"; \
	  echo; \
	  echo "Add a new numbered migration instead; never edit an applied one."; \
	  exit 1; \
	fi; \
	echo "migrations are append-only."


# ---------------------------------------------------------------------------
# The dev library: a seeded library worth rendering, for the TUI and the CLI.
# ---------------------------------------------------------------------------

# The dev library. `dev-data/` is gitignored and reproducible from a seed, so it
# is disposable by design — `make dev-db` always rebuilds it from scratch rather
# than migrating what is there, because a half-seeded database is harder to
# diagnose than a missing one.
DEV_DB_DIR ?= $(CURDIR)/dev-data
DEV_DB_SEED ?= 42
DEV_DB_SRC := corpus/generated/devdb

dev-db: ## Build a seeded library at $(DEV_DB_DIR) — ~200 books, covers, a vault
	@command -v sqlite3 >/dev/null || { \
	  echo "dev-db needs the sqlite3 CLI (present on macOS and ubuntu runners)."; exit 1; }
	@# --data-dir, and not only --out: `cover_path` is a whole path in this
	@# schema, so the seed has to name where the library is being built.
	cargo run -q -p corpus -- gen-devdb --seed $(DEV_DB_SEED) --data-dir "$(DEV_DB_DIR)"
	rm -rf "$(DEV_DB_DIR)"
	mkdir -p "$(DEV_DB_DIR)/database/images" "$(DEV_DB_DIR)/vault"
	@# The engine creates and migrates the database, so corpus never owns a second
	@# copy of the schema or of sqlx's `_sqlx_migrations` ledger. Any read-only
	@# command does it; `list` is the cheapest and prints the empty-library line.
	cargo run -q -p bookmole-cli -- --data-dir "$(DEV_DB_DIR)" list >/dev/null
	cp $(DEV_DB_SRC)/covers/*.png "$(DEV_DB_DIR)/database/images/"
	cp $(DEV_DB_SRC)/vault/*.md "$(DEV_DB_DIR)/vault/"
	sqlite3 "$(DEV_DB_DIR)/database/app.db" < $(DEV_DB_SRC)/seed.sql
	@# The seed lands after the engine migrated an empty database, so 0027's
	@# back-fill (every book that existed got a copy) never saw these books, and
	@# a library of books with no copy is a library with nothing in it. Replay
	@# that back-fill — the migration's own tail, read from the migration, so
	@# there is no second copy of it here — exactly as an upgraded library got it.
	sed -n '/^-- A\. calibre/,$$p' crates/engine/migrations/0027_copies.sql \
	  | sqlite3 "$(DEV_DB_DIR)/database/app.db"
	@# The seed states `cover_path` and cannot state `cover_width` — SQLite cannot
	@# decode a PNG, which is why measuring covers is a CLI command. Without this
	@# every book here has a cover and a NULL `cover_aspect`, and a shelf reading
	@# that column concludes the column does not work.
	cargo run -q -p bookmole-cli -- --data-dir "$(DEV_DB_DIR)" covers
	@# Same shape, same reason, one layer over: the seed states `title` and
	@# `authors` and cannot state the keys they file under, because `sort_title`
	@# drops a leading article and `sort_author` is a parse of a human name.
	@# Without this every book here sorts to the top of `list --sort author`.
	cargo run -q -p bookmole-cli -- --data-dir "$(DEV_DB_DIR)" sort-keys
	@# reading_events comes from the engine's own fillers rather than from invented
	@# rows — the fixture states highlights and readings, and the real derivation
	@# turns them into a log. A generator writing that table directly would be
	@# asserting the activity log's arithmetic instead of exercising it.
	cargo run -q -p bookmole-cli -- --data-dir "$(DEV_DB_DIR)" activity --refill
	@echo ""
	@# No counts here on purpose: `gen-devdb` prints them above and is the only
	@# thing that knows them. A hand-written "220 books, 20 of them" here went
	@# stale the moment the generator gained a case, and nothing said so.
	@echo "dev library at $(DEV_DB_DIR) — counts above, from the generator."
	@echo "What each edge case is for: $(DEV_DB_SRC)/manifest.json"

check: fmt-check lint build-check deny migrations-check test ## Local gate: everything CI runs

# What .github/workflows/ci.yml runs, in the same order. Kept as a separate
# target from `check` even when the two agree: it is the name people reach for,
# and they diverge the moment the workflow grows a step with no local equivalent.
# Keep them in step — a change here belongs in the workflow too.
#
# CI's macOS leg runs `test-engine` rather than `test`; that asymmetry is
# explained in the workflow and is not worth reproducing locally.
#
# `deny` joined because it IS a CI job (`deny` in ci.yml) and `ci` claiming to
# reproduce the gate while skipping one of its jobs was simply wrong.
# `migrations-check` deliberately did NOT: CI runs it as its own job gated
# `if: github.event_name == 'pull_request'`, so it is not part of the gate a
# push to main passes through, and `check` is where it belongs instead.
ci: fmt-check lint build-check deny test ## Reproduce the CI gate locally

test-engine: ## Engine tests only, full proptest count — CI's macOS leg
	$(RUN) -p bookmole --locked

corpus: ## Fetch Gutenberg epubs, generate tier-2 sidecars, run the corpus tests
	scripts/fetch-corpus.sh
	cargo run -p corpus -- gen-corpus --seed $(CORPUS_SEED)
	cargo test -p bookmole --test corpus -- --nocapture

corpus-check: ## Corpus tests only (offline; skips loudly if not generated)
	cargo test -p bookmole --test corpus -- --nocapture

# ---------------------------------------------------------------------------
# The testing corpus: generated layers (KOReader sidecars and statistics, a
# calibre library, a reader's volume) and the recipes that build a library from
# them through the CLI. corpus/recipes/README.md describes a recipe. RECIPE is a
# name under corpus/recipes/ or a path to a recipe file of your own.
# ---------------------------------------------------------------------------

RECIPE ?= everything
DIR ?= $(CURDIR)/target/corpus/$(notdir $(basename $(RECIPE)))
CALIBREDB := $(shell command -v calibredb 2>/dev/null)
SQLITE3 := $(shell command -v sqlite3 2>/dev/null)

corpus-layers: ## Generate every corpus layer in order (KOReader, calibre, reader)
	scripts/fetch-corpus.sh
	cargo run -q -p corpus -- gen-corpus --seed $(CORPUS_SEED)
ifdef CALIBREDB
	cargo run -q -p corpus -- gen-calibre --seed $(CORPUS_SEED)
else
	@echo "SKIPPED: gen-calibre — calibredb not installed; the committed calibre layer stays as it is, and a recipe with a calibre-import step builds without it (its lock is then skipped, not compared)."
endif
	cargo run -q -p corpus -- gen-reader --seed $(CORPUS_SEED)

corpus-build: ## Build RECIPE into DIR through the CLI; compare its semantic lock (EXPECT=update rewrites it)
ifdef SQLITE3
	cargo build -q -p bookmole-cli -p corpus
	rm -rf "$(DIR)"
	$(or $(CARGO_TARGET_DIR),target)/debug/corpus build-recipe "$(RECIPE)" --data-dir "$(DIR)" $(if $(filter update,$(EXPECT)),--update-expect,)
else
	@echo "SKIPPED: corpus-build needs the sqlite3 CLI to count what it built."
endif

corpus-verify: ## The recipe invariant suite on a fresh build of RECIPE; fails if a layer is missing
ifdef SQLITE3
	BOOKMOLE_RECIPE="$(RECIPE)" BOOKMOLE_REQUIRE_FIXTURES=1 cargo test -p bookmole-cli --test recipes -- --nocapture
else
	@echo "SKIPPED: corpus-verify needs the sqlite3 CLI."
endif

corpus-all: ## Layers, then build and verify every committed recipe
	$(MAKE) corpus-layers
	@for r in corpus/recipes/*.toml; do \
	  n=$$(basename $$r .toml); echo "== $$n"; \
	  $(MAKE) --no-print-directory corpus-verify RECIPE=$$n || exit 1; \
	done

synthetic: ## Regenerate the committed tier-1 hostile fixtures, then the goldens
	cargo run -p corpus -- gen-synthetic
	$(MAKE) golden

goodreads: ## Regenerate the committed Goodreads export fixture
	cargo run -p corpus -- gen-goodreads --seed $(CORPUS_SEED)

kostats: ## Regenerate the committed KOReader statistics fixture (SQL + expected totals)
	cargo run -p corpus -- gen-kostats --seed $(CORPUS_SEED)

nightly: ## Reproduce .github/workflows/scheduled.yml locally: advisories, floating-stable clippy, tier-2 corpus
	@# The scheduled workflow was red for ten nights (2026-10-07 to 16) and
	@# nothing local said so — `gh run list` is the other half of this check,
	@# since this target can only run what scheduled.yml runs, not read what it
	@# last reported.
ifdef DENY
	cargo deny check advisories
else
	@echo "SKIPPED: cargo-deny not installed — 'cargo install cargo-deny'."
endif
	@# Advisory only, same as scheduled.yml's `continue-on-error: true` — this
	@# exists so a new lint on floating stable is visible before
	@# rust-toolchain.toml's pin is bumped, never so it blocks anything. The
	@# leading `-` is `continue-on-error` read by `make`.
	@echo "clippy on floating stable (advisory, mirrors continue-on-error):"
	-RUSTUP_TOOLCHAIN=stable cargo clippy --workspace --all-targets -- -D warnings
	$(MAKE) corpus-check
	@# The recipe invariant suite on the whole corpus. Nightly and never `ci`:
	@# it needs a minute or more of debug-build imports.
	$(MAKE) corpus-verify RECIPE=everything

# ---------------------------------------------------------------------------
# Coverage, mutation testing, fuzzing. All three are slow enough, or need a
# toolchain odd enough (nightly, llvm-tools), that they do not belong on the
# PR gate — they are here for the same reason `bench` is: a repeatable local
# entry point for a question `make ci` was never meant to answer.
# ---------------------------------------------------------------------------

COVERAGE_ARGS ?=

coverage: ## Engine line coverage (cargo-llvm-cov + nextest)
ifdef LLVM_COV
	cargo llvm-cov nextest -p bookmole --locked --summary-only $(COVERAGE_ARGS)
else
	@echo "SKIPPED: cargo-llvm-cov not installed — 'cargo install cargo-llvm-cov'."
endif

coverage-html: ## Same, as a browsable report under target/llvm-cov/html
ifdef LLVM_COV
	cargo llvm-cov nextest -p bookmole --locked --html $(COVERAGE_ARGS)
	@echo "report: target/llvm-cov/html/index.html"
else
	@echo "SKIPPED: cargo-llvm-cov not installed — 'cargo install cargo-llvm-cov'."
endif

MUTANTS_FILE ?=
MUTANTS_ARGS ?=

# `--in-place`, because cargo-mutants' default copies the whole tree into
# $TMPDIR to build mutants in isolation, and where $TMPDIR is a size-capped
# tmpfs (common on Linux) a checkout with large target/ directories does not
# fit: the copy dies with "Disk quota exceeded" before a single mutant runs.
# In-place means cargo-mutants edits and restores this checkout instead — fine
# for a checkout nothing else is building in, which is the only place this
# target should run.
mutants: ## Mutation testing (SLOW — 2566 mutants, ~hours unscoped; MUTANTS_FILE=foo.rs to scope)
ifdef MUTANTS
	cargo mutants -p bookmole --test-tool nextest --in-place \
	  $(if $(MUTANTS_FILE),-f $(MUTANTS_FILE)) $(MUTANTS_ARGS)
else
	@echo "SKIPPED: cargo-mutants not installed — 'cargo install cargo-mutants'."
endif

mutants-diff: ## Mutation testing scoped to lines this branch changed vs main
ifdef MUTANTS
	@tmp=$$(mktemp); \
	git diff "$$(git merge-base origin/main HEAD)" > "$$tmp"; \
	cargo mutants -p bookmole --test-tool nextest --in-place --in-diff "$$tmp" $(MUTANTS_ARGS); \
	rm -f "$$tmp"
else
	@echo "SKIPPED: cargo-mutants not installed — 'cargo install cargo-mutants'."
endif

FUZZ_TARGET ?= parse_sidecar
FUZZ_SECONDS ?= 60

fuzz: ## Run one fuzz target for FUZZ_SECONDS (needs a nightly toolchain; never a PR gate)
	@# Flags mirror fuzz.yml / fuzz/README.md: `-s none` because ASAN cannot
	@# survive mlua's longjmp error propagation here; the explicit host triple
	@# because cargo-fuzz's prebuilt binary defaults to musl, which has no std
	@# for this nightly; `-timeout=10` is how a non-terminating Lua chunk is
	@# caught at all; `-rss_limit_mb=2048` matches the CI job.
ifdef CARGO_FUZZ
	mkdir -p fuzz/corpus/$(FUZZ_TARGET)
	cargo +nightly fuzz run -s none --target x86_64-unknown-linux-gnu $(FUZZ_TARGET) -- \
	  -max_total_time=$(FUZZ_SECONDS) -timeout=10 -rss_limit_mb=2048 \
	  fuzz/corpus/$(FUZZ_TARGET) fuzz/seeds/$(FUZZ_TARGET)
else
	@echo "SKIPPED: cargo-fuzz not installed — 'cargo install cargo-fuzz'."
endif

dist: ## Build this machine's release archive into dist/, exactly as CI would
	@set -eu; \
	target=$$(rustc -vV | sed -n 's|^host: ||p'); \
	version=v$$(cargo pkgid -p bookmole-tui | sed 's/.*[#@]//'); \
	name="bookmole-$${version}-$${target}"; \
	cargo build --profile dist --locked --target "$$target" -p bookmole-tui -p bookmole-cli; \
	rm -rf "dist/$${name}"; mkdir -p "dist/$${name}"; \
	cp "target/$${target}/dist/bookmole-tui" "target/$${target}/dist/bookmole" \
	   LICENSE README.md "dist/$${name}/"; \
	tar -C dist -czf "dist/$${name}.tar.gz" "$${name}"; \
	rm -rf "dist/$${name}"; \
	ls -lh "dist/$${name}.tar.gz"

clean: ## cargo clean
	cargo clean
