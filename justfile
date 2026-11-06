# Recipes assume the dev shell is active: nix develop --command just <recipe>

# ==============================================================================
# Formatting
# ==============================================================================

fmt:
    nix fmt

fmt-check:
    nix fmt -- --ci

# ==============================================================================
# Lint
# ==============================================================================

clippy:
    cargo clippy --all-targets -- --deny warnings

audit:
    cargo audit

deny:
    cargo deny check

markdownlint:
    markdownlint-cli2 "**/*.md" "#.beads" "#.agents" "#target" "#.direnv" "#mutants.out*" "#AGENTS.md" "#CLAUDE.md" "#reference"

lint: fmt-check clippy audit deny markdownlint

# ==============================================================================
# Test
# ==============================================================================

test:
    cargo test

mutants *args:
    cargo mutants --jobs 8 --timeout 60 {{args}}

# A release tag has to name the version the binary reports.
check-tag tag:
    #!/usr/bin/env bash
    set -euo pipefail
    tag={{quote(tag)}}
    reported=$(cargo run --quiet -- --version)
    [[ "$tag" == "v${reported#tab-keeper }" ]] || {
      echo "tag $tag does not match the binary, which reports: $reported" >&2
      exit 1
    }

check: lint test
