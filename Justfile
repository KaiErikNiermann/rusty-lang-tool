set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

REPO := "KaiErikNiermann/rusty-lang-tool"
PAGES_URL := "https://kaierikniermann.github.io/rusty-lang-tool/"

# List recipes
default:
    @just --list

# --- Development setup ---

# Point git at the repo's pre-push hooks (run once after cloning)
dev:
    git config core.hooksPath .githooks
    @echo "pre-push hooks enabled (.githooks)"

# --- Rust: build / test / lint ---

# Build the whole workspace
build:
    cargo build --workspace

# Run the workspace test suite (the LT-data oracle tests are #[ignore]d — see `oracle`)
test:
    cargo test --workspace

# Clippy, warnings-as-errors (the house gate)
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Apply rustfmt
fmt:
    cargo fmt --all

# Check rustfmt (informational — the tree predates fmt enforcement)
fmt-check:
    cargo fmt --all --check

# Every configured language is wired into all sites (no fetched data needed)
coherence:
    cargo run -q -p xtask -- lang-coherence

# Repo-specific semgrep rules — conventions + reuse (see .semgrep/README.md)
semgrep:
    # `--error` makes any finding a non-zero exit, so this can gate.
    poetry run semgrep --config .semgrep/ --error --metrics=off .

# Install the pinned semgrep (Poetry, py3.12) — run once after cloning
semgrep-setup:
    poetry install --no-interaction

# ESLint over web/ — sonarjs + unicorn + security (semgrep cannot parse .svelte)
web-lint:
    cd web && pnpm install --frozen-lockfile && pnpm run lint

# The blocking gate mirrored from CI / pre-push (clippy + semgrep + coherence + tests)
check: clippy semgrep coherence test

# --- LanguageTool data & artifacts ---

# Fetch pinned LT sources + schemas (resumable)
fetch-lt:
    cargo run -p xtask -- fetch-lt

# Fetch the nlprule baseline engine binaries (resumable)
fetch-engine:
    cargo run -p xtask -- fetch-engine

# The configured language codes (en de ru ar fr es it)
langs:
    @cargo run -q -p xtask -- lang-codes

# Build one language's native artifacts (tagger + disambig + grammar blob, + synthesizer where configured)
build-lang lang:
    cargo run -p xtask -- build-lang --lang {{lang}}

# Build one language's word-form synthesis artifact (suggestions that need inflected forms)
build-synth lang="de":
    cargo run -p xtask -- build-synth --lang {{lang}}

# Build every configured language's artifacts
build-all:
    for c in $(cargo run -q -p xtask -- lang-codes); do \
        echo "::: build-lang $c"; cargo run -p xtask -- build-lang --lang "$c"; \
    done

# Grammar-rule health audit for a language (fidelity gate + firing-rate + convergence/no-op scan)
audit lang="en":
    cargo run -p xtask -- audit-rules --lang {{lang}}

# Compare with LanguageTool itself on its examples and attribute every gap (needs java 21+, gradle)
coverage lang="de":
    cargo run --release -p xtask -- coverage --lang {{lang}}

# Score the example-corpus oracle for both backends (the on-thesis gate; needs built artifacts)
oracle:
    cargo test -p rlt-cli --test oracle --release -- --ignored --nocapture --test-threads 1

# --- Web demo ---

# Build the wasm pkg consumed by the web app (--target web, no nlprule)
wasm:
    pnpm --prefix web run wasm

# Run the dev server (wasm-packs + stages English artifacts first)
web-dev:
    cd web && pnpm install && pnpm run dev

# Stage artifacts + manifest into web/static (default English; `all` for every built language)
web-stage langs="en":
    cd web && node scripts/stage-artifacts.mjs {{langs}}

# Strict svelte-check + vitest + the production build gate
web-check:
    cd web && pnpm install && pnpm run check && pnpm test && pnpm exec vite build

# Static production build into web/build
web-build:
    cd web && pnpm install && pnpm run build

# --- Backlog board ---
#
# Tasks live in backlog/ (gitignored: local planning, not shared). The board is a fork of Backlog.md
# that adds a Graph view; it serves this project on port 6421 so it can run beside other projects'
# boards on the default 6420.

# Checkout of the Backlog.md fork that adds a Graph view to the web board (override: BACKLOG_BOARD_DIR)
board_dir := env("BACKLOG_BOARD_DIR", home_directory() / "Projects/ext-projects/Backlog.md")

# Build the board fork (needs bun 1.4.4); run again after pulling or editing the fork
board-build:
    cd {{ board_dir }} && bun install --frozen-lockfile --ignore-scripts && bun scripts/build.ts

# Open this project's Backlog board (LAN host + printed ?token= URL; `just board 6421 127.0.0.1` stays local)
board port="6421" host=env("BACKLOG_BOARD_HOST", ""):
    #!/usr/bin/env bash
    set -euo pipefail
    # On a non-loopback address the server refuses every request without a session token: open the
    # `?token=` URL it prints once per browser (it sets a cookie), or send `Authorization: Bearer
    # <token>`. The token is random per start; BACKLOG_BROWSER_TOKEN (16+ characters) keeps it across
    # restarts. Anyone who holds it can edit tasks, so treat the URL as a password.
    test -x {{ board_dir }}/dist/backlog || { echo "no build in {{ board_dir }}: run just board-build (or set BACKLOG_BOARD_DIR)" >&2; exit 1; }
    test -f backlog/config.yml || { echo "no backlog/ here: run {{ board_dir }}/dist/backlog init" >&2; exit 1; }
    host={{ quote(host) }}
    if [ -z "$host" ]; then
        # The source address of the default route, i.e. this machine's LAN address.
        host=$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}' || true)
        host=${host:-127.0.0.1}
    fi
    echo "board on http://$host:{{ port }} (open the ?token= URL printed below unless the host is 127.0.0.1)" >&2
    exec {{ board_dir }}/dist/backlog browser --port {{ port }} --host "$host" --no-open

# --- Versioning / deploy (the GitHub Pages demo) ---
#
# The "release" here is the per-language artifact bundle published to the
# `artifacts-<LT version>` GitHub Release; the site fetches it same-origin after
# deploy-pages bakes it in. These recipes abstract the multi-step dance so you
# don't run the gh workflow incantations by hand.

# The pinned LanguageTool version (drives the artifacts-<ver> release tag)
lt-version:
    @grep -oP 'const LT_VERSION: &str = "\K[^"]+' xtask/src/main.rs

# Recent CI / deploy / release runs + open dependency PRs
status:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "── recent workflow runs ──"
    gh run list --repo {{REPO}} --limit 8
    echo; echo "── open PRs ──"
    gh pr list --repo {{REPO}}

# Dispatch the artifact rebuild + republish workflow (does NOT deploy the site)
release-artifacts:
    gh workflow run release-artifacts.yml --repo {{REPO}}

# Dispatch the Pages deploy workflow (uses whatever artifacts are released)
deploy:
    gh workflow run deploy-pages.yml --repo {{REPO}}

# Cancel any in-flight Pages deploy (so new wasm isn't published against old artifacts)
cancel-deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    id=$(gh run list --workflow deploy-pages.yml --repo {{REPO}} --limit 1 \
            --json databaseId,status -q '.[] | select(.status != "completed") | .databaseId')
    if [ -n "${id:-}" ]; then
        gh run cancel "$id" --repo {{REPO}}
        echo "cancelled in-flight deploy $id"
    else
        echo "no in-flight deploy"
    fi

# Full redeploy for an engine/IR change: rebuild+republish ALL artifacts, then deploy Pages
redeploy:
    #!/usr/bin/env bash
    set -euo pipefail
    just cancel-deploy
    echo "→ rebuilding + republishing language artifacts (release-artifacts.yml)…"
    gh workflow run release-artifacts.yml --repo {{REPO}}
    just _wait release-artifacts.yml
    echo "→ deploying Pages (deploy-pages.yml)…"
    gh workflow run deploy-pages.yml --repo {{REPO}}
    just _wait deploy-pages.yml
    @echo "✓ live: {{PAGES_URL}}"

# Redeploy the site only (web/wasm change, no artifact-format change; reuses released artifacts)
redeploy-web:
    #!/usr/bin/env bash
    set -euo pipefail
    gh workflow run deploy-pages.yml --repo {{REPO}}
    just _wait deploy-pages.yml
    @echo "✓ live: {{PAGES_URL}}"

# Internal: wait for the most recent run of a workflow to finish (after a dispatch)
_wait workflow:
    #!/usr/bin/env bash
    set -euo pipefail
    sleep 6
    id=$(gh run list --workflow "{{workflow}}" --repo {{REPO}} --limit 1 --json databaseId -q '.[0].databaseId')
    echo "  watching {{workflow}} run $id…"
    gh run watch "$id" --repo {{REPO}} --exit-status
