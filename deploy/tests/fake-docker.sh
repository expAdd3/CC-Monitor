#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  inspect)
    printf '%s\n' "${FAKE_DOCKER_STATE:-healthy}"
    ;;
  exec)
    shift

    while [[ ${1:-} == -* ]]; do
      case "$1" in
        -e | --env)
          [[ $# -ge 2 ]] || exit 2
          shift 2
          ;;
        --env=*)
          shift
          ;;
        *)
          exit 2
          ;;
      esac
    done

    [[ $# -ge 3 ]] || exit 2
    container=$1
    executable=$2
    shift 2
    [[ -n $container && $executable == ntfy ]] || exit 2

    case "${1:-}" in
      user)
        shift
        case "${1:-}" in
          add)
            shift
            [[ $# -eq 2 && $2 == --password-from-env ]] || exit 2
            username=$1
            printf 'user-add\t%s\n' "$username" >>"$FAKE_DOCKER_LOG"
            if [[ -n ${FAKE_DOCKER_EXISTING_USER:-} ]] &&
              [[ $username == "$FAKE_DOCKER_EXISTING_USER" ]]; then
              exit 1
            fi
            ;;
          del)
            shift
            [[ $# -eq 1 ]] || exit 2
            printf 'user-del\t%s\n' "$1" >>"$FAKE_DOCKER_LOG"
            ;;
          *)
            exit 2
            ;;
        esac
        ;;
      access)
        shift
        [[ ${1:-} == -- ]] || exit 2
        shift
        [[ $# -eq 3 ]] || exit 2
        username=$1
        topic=$2
        permission=$3
        printf 'access\t%s\t%s\t%s\n' \
          "$username" "$topic" "$permission" >>"$FAKE_DOCKER_LOG"
        if [[ ${FAKE_DOCKER_FAIL_ACCESS:-0} == 1 ]]; then
          exit 1
        fi
        ;;
      *)
        exit 2
        ;;
    esac
    ;;
  *)
    exit 2
    ;;
esac
