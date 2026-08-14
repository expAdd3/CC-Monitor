#!/bin/sh
set -eu

repository="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
checker="$repository/scripts/check-release-worktree.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

git -C "$fixture" init -q
git -C "$fixture" config user.name "CC Monitor Test"
git -C "$fixture" config user.email "cc-monitor-test@example.invalid"
printf 'tracked\n' > "$fixture/tracked.txt"
git -C "$fixture" add tracked.txt
git -C "$fixture" commit -qm "fixture"

expected="$(git -C "$fixture" rev-parse HEAD)"
output="$(sh "$checker" "$fixture")"
if [ "$output" != "Release source commit: $expected" ]; then
  echo "clean worktree did not report its exact commit" >&2
  exit 1
fi

printf 'changed\n' >> "$fixture/tracked.txt"
if sh "$checker" "$fixture" >/dev/null 2>&1; then
  echo "tracked changes must fail the release worktree check" >&2
  exit 1
fi
git -C "$fixture" restore tracked.txt

printf 'untracked\n' > "$fixture/untracked.txt"
if sh "$checker" "$fixture" >/dev/null 2>&1; then
  echo "untracked files must fail the release worktree check" >&2
  exit 1
fi

echo "release worktree contract tests passed"
