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
    markdownlint-cli2 "**/*.md" "#.beads" "#.agents" "#target" "#.direnv" "#mutants.out*" "#AGENTS.md" "#CLAUDE.md"

lint: fmt-check clippy audit deny markdownlint

# ==============================================================================
# Test
# ==============================================================================

test: test-rust test-extension

test-rust:
    cargo test

test-extension:
    node --test extension/tests/*.test.mjs

# Signs extension/tab-keeper.crx. Needs the e2e shell and an age identity for the key.
pack:
    extension/pack.sh

# The whole install on a fresh NixOS VM, from the flake's own modules. Needs KVM.
vm-test:
    nix build .#vm-test --print-build-logs

# The same on branded Google Chrome, including the memory discard frees. Needs KVM.
vm-test-chrome:
    nix build .#vm-test-chrome --print-build-logs

# Against real Chromium. Needs its own shell: nix develop .#e2e --command just e2e
e2e *args:
    cargo build
    node --test --test-concurrency=1 --test-timeout=120000 {{args}} tests/e2e/*.test.mjs

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
