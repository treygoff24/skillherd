#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
GATE="$ROOT/tests/shell-check.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/skillherd-gate-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

for fixture in clean broken missing empty; do
  mkdir -p "$TMP/$fixture/bin/path with spaces" "$TMP/$fixture/tests"
done

printf '%s\n' '#!/bin/bash' 'exit 0' > "$TMP/clean/bin/path with spaces/tool"
chmod 500 "$TMP/clean/bin/path with spaces/tool"
printf '%s\n' '#!/bin/bash' 'exit 0' > "$TMP/clean/tests/non-executable.sh"
chmod 600 "$TMP/clean/tests/non-executable.sh"
printf '%s\n' '#!/bin/bash' 'if then' > "$TMP/clean/bin/not-selected"
chmod 600 "$TMP/clean/bin/not-selected"
printf '%s\n' '#!/bin/bash' 'if then' > "$TMP/broken/bin/path with spaces/broken tool"
chmod 500 "$TMP/broken/bin/path with spaces/broken tool"
rmdir "$TMP/missing/tests"

if output=$(bash "$GATE" "$TMP/clean" 2>&1) \
  && [[ "$output" == *'All 2 shell scripts pass syntax check.'* ]]; then
  ok 'enumerates executable and .sh files without expanding selection scope'
else
  bad 'enumerates executable and .sh files without expanding selection scope'
fi

if output=$(bash "$GATE" "$TMP/broken" 2>&1); then
  bad 'rejects broken executable at path with spaces'
elif [[ "$output" == *'SYNTAX ERROR: bin/path with spaces/broken tool'* ]]; then
  ok 'rejects broken executable at path with spaces'
else
  bad 'rejects broken executable at path with spaces'
fi

if output=$(bash "$GATE" "$TMP/missing" 2>&1); then
  bad 'propagates missing-root enumeration failure'
elif [[ "$output" == *'SCRIPT ENUMERATION FAILED'* ]]; then
  ok 'propagates missing-root enumeration failure'
else
  bad 'propagates missing-root enumeration failure'
fi

if output=$(bash "$GATE" "$TMP/empty" 2>&1); then
  bad 'rejects zero scripts checked'
elif [[ "$output" == *'NO SHELL SCRIPTS CHECKED'* ]]; then
  ok 'rejects zero scripts checked'
else
  bad 'rejects zero scripts checked'
fi

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
