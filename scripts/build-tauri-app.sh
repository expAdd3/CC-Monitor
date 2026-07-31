#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

./scripts/prepare-hook-sidecar.sh release
cargo tauri build --bundles app

bundle_path="target/release/bundle/macos/CC Monitor.app"
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

echo "Local acceptance bundle: $bundle_path"
