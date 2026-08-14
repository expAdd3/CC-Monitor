#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/prepare-hook-sidecar.sh release
cargo tauri build --bundles app

bundle_path="target/release/bundle/macos/CC Monitor.app"
main_binary="$bundle_path/Contents/MacOS/cc-monitor"
hook_binary="$bundle_path/Contents/MacOS/cc-monitor-hook"

if [ ! -x "$main_binary" ] || [ ! -x "$hook_binary" ]; then
  echo "Bundle is missing an executable main binary or Hook sidecar" >&2
  exit 1
fi

main_architectures="$(lipo -archs "$main_binary")"
hook_architectures="$(lipo -archs "$hook_binary")"
if [ "$main_architectures" != "$hook_architectures" ]; then
  echo "Bundle architecture mismatch: app=$main_architectures hook=$hook_architectures" >&2
  exit 1
fi

# UNUserNotificationCenter associates permissions with the signed application
# identity. Keep the configured bundle identifier and the resulting code-sign
# identifier aligned instead of repairing an invalid artifact after the build.
bundle_identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle_path/Contents/Info.plist")"
signature_identifier="$(codesign -dvv "$bundle_path" 2>&1 | sed -n 's/^Identifier=//p')"

if [ "$bundle_identifier" != "com.ccmonitor" ] || [ "$signature_identifier" != "$bundle_identifier" ]; then
  echo "Bundle identity mismatch: plist=$bundle_identifier signature=$signature_identifier" >&2
  exit 1
fi

codesign --verify --deep --strict "$bundle_path"

echo "Bundle architecture: $main_architectures"
echo "Local acceptance bundle: $bundle_path"
