#!/usr/bin/env bash
#
# Everything CI would run, in the order that fails fastest.
#
#   ./scripts/check.sh            format, lint, test
#   ./scripts/check.sh --hardware also run the tests that need a real display
#
# Tests needing a display, a keyboard or a network are marked #[ignore] so this
# passes on a headless box. --hardware runs those too, and will briefly move
# your mouse and capture a frame of your screen.
set -euo pipefail

cd "$(dirname "$0")/.."
HARDWARE=${1:-}

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$1"; }

step "Formatting"
cargo fmt --all --check

step "Lints (warnings are errors)"
cargo clippy --workspace --all-targets -- -D warnings

step "Tests"
cargo test --workspace

if [ "$HARDWARE" = "--hardware" ]; then
  step "Hardware tests (moves the cursor, captures a frame)"
  cargo test --workspace -- --ignored
fi

step "Release build"
cargo build --workspace --release

printf '\n\033[1;32mAll checks passed.\033[0m\n'
printf 'Binaries: target/release/remu (desk app), target/release/remu-relay (server)\n'
