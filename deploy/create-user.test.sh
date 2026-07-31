#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT
mkdir "$test_root/bin"
ln -s "$repo_root/deploy/tests/fake-docker.sh" "$test_root/bin/docker"
export PATH="$test_root/bin:$PATH"
export FAKE_DOCKER_LOG="$test_root/docker.log"
fixture="$repo_root/tests/fixtures/ntfy_topics.json"

run_create_user() {
  "$repo_root/deploy/create-user.sh" "${2:-alice}" secret "$1" >/dev/null 2>&1
}

fixture_topics() {
  bun -e \
    'const fixture = await Bun.file(process.argv[1]).json(); for (const topic of fixture[process.argv[2]]) console.log(topic);' \
    "$fixture" "$1"
}

valid_index=0
while IFS= read -r topic; do
  valid_index=$((valid_index + 1))
  run_create_user "$topic" "fixture-$valid_index"
done < <(fixture_topics valid)

while IFS= read -r topic; do
  if run_create_user "$topic"; then
    echo "expected invalid topic to fail: $topic" >&2
    exit 1
  fi
done < <(fixture_topics invalid)

: >"$FAKE_DOCKER_LOG"
run_create_user "-alerts" leading-dash
if ! grep -Fxq -- $'access\tleading-dash\t-alerts\tread-write' "$FAKE_DOCKER_LOG"; then
  echo "leading-hyphen topic was not passed as an exact positional argument" >&2
  exit 1
fi

: >"$FAKE_DOCKER_LOG"
FAKE_DOCKER_STATE=stopped
export FAKE_DOCKER_STATE
if run_create_user alerts; then
  echo "expected a stopped container to fail" >&2
  exit 1
fi
if [[ -s $FAKE_DOCKER_LOG ]]; then
  echo "stopped-container check mutated Docker state" >&2
  exit 1
fi

FAKE_DOCKER_STATE=healthy
FAKE_DOCKER_FAIL_ACCESS=1
export FAKE_DOCKER_STATE FAKE_DOCKER_FAIL_ACCESS
if run_create_user alerts; then
  echo "expected ACL failure" >&2
  exit 1
fi
if ! grep -Fxq -- $'user-del\talice' "$FAKE_DOCKER_LOG"; then
  echo "ACL failure did not roll back the created user" >&2
  exit 1
fi

: >"$FAKE_DOCKER_LOG"
unset FAKE_DOCKER_FAIL_ACCESS
FAKE_DOCKER_EXISTING_USER=existing-user
export FAKE_DOCKER_EXISTING_USER
if run_create_user alerts existing-user; then
  echo "expected an existing user to fail in one-shot mode" >&2
  exit 1
fi
if grep -Eq '^(access|user-del)	' "$FAKE_DOCKER_LOG"; then
  echo "existing-user failure mutated ACLs or deleted the existing user" >&2
  exit 1
fi
if ! grep -Fxq -- $'user-add\texisting-user' "$FAKE_DOCKER_LOG"; then
  echo "existing-user contract did not attempt create-only user add" >&2
  exit 1
fi

echo "create-user contract tests passed"
