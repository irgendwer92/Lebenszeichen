#!/bin/sh
# Entry point of the image.
#
# Plain Docker: runs lz-server with the arguments given (the CMD).
# Home Assistant app (the Supervisor sets SUPERVISOR_TOKEN and writes
# /data/options.json): builds the arguments from the app options and ignores
# the CMD.
set -eu

if [ -z "${SUPERVISOR_TOKEN:-}" ] || [ ! -r /data/options.json ]; then
    exec lz-server "$@"
fi

http=$(jq -r '.http // false' /data/options.json)
relay=$(jq -r '.relay // ""' /data/options.json)

umask 077
mkdir -p /share/lebenszeichen

set -- --state /data/device.json \
    --socket /share/lebenszeichen/lz.sock \
    --page 0.0.0.0:8099 --page-allow 172.30.32.2
if [ "$http" = "true" ]; then
    set -- "$@" --http 0.0.0.0:8080
fi
if [ -n "$relay" ]; then
    set -- "$@" --relay "$relay"
fi
exec lz-server "$@"
