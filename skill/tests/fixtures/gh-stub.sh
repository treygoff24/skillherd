#!/usr/bin/env bash
# Stub `gh` for the release tests.
#
# It is copied to `<home>/bin/gh` (with `<home>/bin/gh.conf` beside it) and
# stands in for the two `gh api` calls a release fetch makes:
#
#   api repos/<o>/<r>/releases?per_page=100&page=N   cat <listing_dir>/page-N.json
#                                                    (`[]` when there is none)
#   api -H 'Accept: …' repos/<o>/<r>/releases/assets/<id>
#                                                    cat <asset_dir>/<id>.zip
#
# It records the `HOME`, working directory and argv of every call.
# gh.conf keys: log, exit, stderr, listing_dir, asset_dir.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
conf="$here/gh.conf"

val() {
  [ -f "$conf" ] || return 0
  sed -n "s/^$1=//p" "$conf" | head -n1
}

log=$(val log)
if [ -n "$log" ]; then
  {
    echo "HOME=$HOME"
    echo "PWD=$PWD"
    echo "ARGV=$*"
  } >> "$log"
fi

code=$(val exit)
[ -z "$code" ] && code=0
if [ "$code" != 0 ]; then
  val stderr >&2
  exit "$code"
fi

if [ "${1:-}" != api ]; then
  echo "stub gh: only \`gh api\` is stubbed" >&2
  exit 2
fi
path=${!#}

case "$path" in
  */releases\?*)
    page=$(printf '%s\n' "$path" | sed -n 's/.*[?&]page=\([0-9][0-9]*\).*/\1/p')
    listing="$(val listing_dir)/page-$page.json"
    if [ -f "$listing" ]; then
      cat "$listing"
    else
      echo '[]'
    fi
    ;;
  */releases/assets/*)
    asset="$(val asset_dir)/${path##*/}.zip"
    if [ ! -f "$asset" ]; then
      echo "gh: Not Found (HTTP 404)" >&2
      exit 1
    fi
    cat "$asset"
    ;;
  *)
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1
    ;;
esac
