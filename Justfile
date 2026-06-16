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

# The blocking gate mirrored from CI / pre-push (clippy + coherence + tests)
check: clippy coherence test

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

# Build one language's native artifacts (tagger + disambig + grammar blob)
build-lang lang:
    cargo run -p xtask -- build-lang --lang {{lang}}

# Build every configured language's artifacts
build-all:
    for c in $(cargo run -q -p xtask -- lang-codes); do \
        echo "::: build-lang $c"; cargo run -p xtask -- build-lang --lang "$c"; \
    done

# Grammar-rule health audit for a language (fidelity gate + firing-rate + convergence/no-op scan)
audit lang="en":
    cargo run -p xtask -- audit-rules --lang {{lang}}

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
