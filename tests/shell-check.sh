#!/usr/bin/env bash
# Syntax-check every shell script in the repo per its shebang, plus shellcheck
# (severity warning) on bash scripts when shellcheck is installed.
#
#   tests/shell-check.sh [repo-dir]   (default: the repo holding this script)
#
# Checked: executable files and *.sh files under bin/, tests/ and, when it
# exists, scripts/, plus install.sh at the root. Exit 1 on any failure, or when no script was found.
ROOT=${1:-$(cd "$(dirname "$0")/.." && pwd)}
cd "$ROOT" || exit 1

FAIL=0
CHECKED=0
FILES=$(mktemp "${TMPDIR:-/tmp}/skillherd-shell-check.XXXXXX") || exit 1
trap 'rm -f "$FILES"' EXIT

DIRS=(bin tests)
[ -d scripts ] && DIRS+=(scripts)
if ! find "${DIRS[@]}" -type f \
  \( -perm -100 -o -perm -010 -o -perm -001 -o -name '*.sh' \) \
  -print0 > "$FILES"; then
  echo "SCRIPT ENUMERATION FAILED" >&2
  exit 1
fi
# The installer sits at the root; check it when present.
[ -f install.sh ] && printf 'install.sh\0' >> "$FILES"

while IFS= read -r -d '' FILE; do
  SHEBANG=$(head -1 "$FILE")
  case "$SHEBANG" in
    *zsh*) INTERP=zsh ;;
    *bash*|*sh*) INTERP=bash ;;
    *) continue ;;
  esac
  CHECKED=$((CHECKED + 1))
  if ! "$INTERP" -n "$FILE" 2>&1; then
    echo "SYNTAX ERROR: $FILE" >&2
    FAIL=1
  fi
  # The linter does not support zsh, so bash only.
  if [ "$INTERP" = bash ] && command -v shellcheck >/dev/null 2>&1; then
    if ! shellcheck --severity=warning "$FILE"; then
      echo "SHELLCHECK FAILED: $FILE" >&2
      FAIL=1
    fi
  fi
done < "$FILES"

[ "$CHECKED" -eq 0 ] && { echo "NO SHELL SCRIPTS CHECKED" >&2; exit 1; }
[ "$FAIL" -eq 1 ] && exit 1
echo "All $CHECKED shell scripts pass syntax check."
exit 0
