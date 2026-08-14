#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/prepare-hook-sidecar.sh debug
sh ./scripts/check-release-worktree.test.sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --all-targets
bun run test
bun run test:deploy
bun run build
git diff --check
