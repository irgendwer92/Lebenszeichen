#!/usr/bin/env bash
# Checks that the Android string resources of every language define exactly
# the names of the default language (values/strings.xml).
#
# Usage: scripts/check-android-strings.sh

set -euo pipefail
res="$(dirname "$0")/../android/kotlin/src/main/res"
names() { grep -oE "<(string|plurals|string-array) name=\"[^\"]+\"" "$1" | sort; }
status=0
for file in "$res"/values-*/strings.xml; do
  if ! diff <(names "$res/values/strings.xml") <(names "$file") >/dev/null; then
    echo "String names differ: $file"; diff <(names "$res/values/strings.xml") <(names "$file") || true; status=1
  fi
done
[ "$status" -eq 0 ] && echo "Android strings consistent."
exit "$status"
