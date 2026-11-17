#!/bin/sh
# Reports a state to Lebenszeichen through its local interface
# (docs/m8-local-interface.md). Needs curl.
#
#   report-state.sh set 480 asleep    the state applies for 480 minutes
#   report-state.sh clear             the state ends now
#   report-state.sh show              prints the current state
#
# Contacts get the state as an answer only if you allowed it for them on
# this device, and only after the waiting time without a manual answer.
#
# The socket of the desktop app is $XDG_RUNTIME_DIR/lebenszeichen/app.sock,
# that of `lz listen` is …/lz.sock. Choose with LZ_SOCKET; a token, if the
# socket needs one, goes into LZ_TOKEN.

set -eu

socket=${LZ_SOCKET:-${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is not set}/lebenszeichen/app.sock}

call() {
    # --fail-with-body (curl 7.76+) fails on an error status and still prints
    # the error text, so the exit code tells whether it worked.
    if [ -n "${LZ_TOKEN:-}" ]; then
        curl -sS --fail-with-body --unix-socket "$socket" -H "Authorization: Bearer $LZ_TOKEN" "$@"
    else
        curl -sS --fail-with-body --unix-socket "$socket" "$@"
    fi
}

case ${1:-} in
set)
    [ $# -ge 3 ] || { echo "usage: $0 set <minutes> <reason>" >&2; exit 2; }
    minutes=$2
    shift 2
    # The reason goes through jq if there is one, so quotes in it are safe.
    if command -v jq >/dev/null; then
        body=$(jq -cn --arg reason "$*" --argjson minutes "$minutes" \
            '{reason: $reason, minutes: $minutes}')
    else
        case $* in *'"'* | *'\'*)
            echo "without jq the reason may not contain quotes or backslashes" >&2
            exit 2
            ;;
        esac
        body="{\"reason\":\"$*\",\"minutes\":$minutes}"
    fi
    call -X POST -H 'Content-Type: application/json' -d "$body" http://lz/state
    ;;
clear)
    call -X DELETE http://lz/state
    ;;
show)
    call http://lz/state
    echo
    ;;
*)
    echo "usage: $0 set <minutes> <reason> | clear | show" >&2
    exit 2
    ;;
esac
