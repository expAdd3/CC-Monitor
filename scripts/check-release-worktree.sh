#!/bin/sh
set -eu

repository="${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}"
status="$(git -C "$repository" status --porcelain --untracked-files=all)"

if [ -n "$status" ]; then
  echo "Release source is not reproducible: commit or stash every tracked and untracked change." >&2
  git -C "$repository" status --short --untracked-files=all >&2
  exit 1
fi

commit="$(git -C "$repository" rev-parse HEAD)"
echo "Release source commit: $commit"
