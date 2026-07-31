#!/bin/sh
set -eu

profile="${1:-debug}"
case "$profile" in
  debug)
    cargo build -p cc-monitor-hook
    ;;
  release)
    cargo build -p cc-monitor-hook --release
    ;;
  *)
    echo "usage: $0 [debug|release]" >&2
    exit 2
    ;;
esac

target_triple="$(rustc -vV | sed -n 's/^host: //p')"
source_path="target/$profile/cc-monitor-hook"
target_dir="src-tauri/binaries"
target_path="$target_dir/cc-monitor-hook-$target_triple"

mkdir -p "$target_dir"
cp "$source_path" "$target_path"
# The bundled helper contains no secrets. Read/execute permission for every
# local account keeps an administrator-built application usable after it is
# copied to a shared /Applications directory.
chmod 755 "$target_path"
