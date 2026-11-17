#!/bin/sh
# Reports a state to Lebenszeichen through its local interface
# (docs/m8-local-interface.md). Needs curl.
#
#   report-state.sh set 480 asleep    the state applies for 480 minutes
#   report-state.sh clear             the state ends now
#   report-state.sh show              prints the current state
#   report-state.sh set --share 1440 Phone empty
#                                     also hands the state to your other devices
#   report-state.sh clear --share     ends it on all your devices
#
# With --share, a state set goes through the encrypted sync mailbox to all
# your devices, and clear ends a shared or handed state everywhere. Without
# it, a state stays on this device and clear only ends the state set here.
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
    shift
    share=false
    if [ "${1:-}" = "--share" ]; then
        share=true
        shift
    fi
    [ $# -ge 2 ] || { echo "usage: $0 set [--share] <minutes> <reason>" >&2; exit 2; }
    minutes=$1
    shift
    # The reason goes through jq if there is one, so quotes in it are safe.
    if command -v jq >/dev/null; then
        body=$(jq -cn --arg reason "$*" --argjson minutes "$minutes" \
            --argjson share "$share" \
            '{reason: $reason, minutes: $minutes} + (if $share then {share: true} else {} end)')
    else
        case $* in *'"'* | *'\'*)
            echo "without jq the reason may not contain quotes or backslashes" >&2
            exit 2
            ;;
        esac
        body="{\"reason\":\"$*\",\"minutes\":$minutes}"
        if [ "$share" = true ]; then
            body="${body%\}},\"share\":true}"
        fi
    fi
    call -X POST -H 'Content-Type: application/json' -d "$body" http://lz/state
    ;;
clear)
    case ${2:-} in
    "") call -X DELETE http://lz/state ;;
    --share) call -X DELETE 'http://lz/state?share=true' ;;
    *) echo "usage: $0 clear [--share]" >&2; exit 2 ;;
    esac
    ;;
show)
    # {"reason":…,"until":…,"shared":…,"handed":…}; the notes are appended.
    if out=$(call http://lz/state); then
        notes=
        case $out in *'"shared":true'*) notes="$notes (shared)" ;; esac
        case $out in *'"handed":true'*) notes="$notes (from another device)" ;; esac
        echo "$out$notes"
    else
        echo "$out"
        exit 1
    fi
    ;;
*)
    echo "usage: $0 set [--share] <minutes> <reason> | clear [--share] | show" >&2
    exit 2
    ;;
esac
