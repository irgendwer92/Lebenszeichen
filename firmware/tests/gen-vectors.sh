#!/bin/sh
# SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception
#
# Writes the test vectors of the Rust reference satellite
# (crates/client/src/tests/reference.rs, vectors()) to
# firmware/tests/vectors/m10.json, which the C tests reproduce.
#
#   firmware/tests/gen-vectors.sh          regenerate the file
#   firmware/tests/gen-vectors.sh --check  fail if the file is not current
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
file="$root/firmware/tests/vectors/m10.json"
out=$(mktemp)
trap 'rm -f "$out"' EXIT

(cd "$root" && LZ_VECTORS_OUT="$out" cargo test --locked -q -p lz-client vectors_are_reproducible >/dev/null)

if [ "${1:-}" = "--check" ]; then
    if ! cmp -s "$out" "$file"; then
        echo "firmware/tests/vectors/m10.json is not what the Rust tests write;" >&2
        echo "run firmware/tests/gen-vectors.sh and commit the result." >&2
        exit 1
    fi
    echo "test vectors are current"
else
    cp "$out" "$file"
    echo "wrote $file"
fi
