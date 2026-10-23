#!/usr/bin/env bash
# install.sh — publish this checkout's tools onto the host (idempotent).
#
#   skill/ (cargo build --release)         -> PREFIX/bin/skillherd (0755)
#   bin/*                                  -> PREFIX/bin/<name>   (0755)
#
# A tool whose header carries `# platform: darwin` (or linux) installs only on
# that platform; elsewhere it is skipped and reported. Real files, not
# symlinks: the live tools keep working while the checkout is dirty or
# mid-rebase. Edit the checkout, then run this again to publish changes.
#
# The `skillherd` binary is built from `skill/Cargo.toml` with the host's cargo
# and installed like any other tool, so `--verify` also reports a stale
# binary. Both install and verify require cargo and a C linker.
#
# Usage: install.sh [--prefix DIR] [--root DIR] [--verify]
#   --prefix DIR install under DIR/bin (default: $HOME/.local)
#   --root DIR   use DIR/.local as the default prefix instead of $HOME/.local
#                an explicit --prefix wins regardless of argument order
#   --verify     install nothing; print one line per item, exit 1 on any drift
#                still builds the binary to compare it with the installed copy
#   SKILLHERD_CLI_PLATFORM=linux|darwin overrides the uname-derived platform (tests)
set -euo pipefail

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
ROOT="$HOME"
PREFIX=""
VERIFY=0
WITH_NPX=0
ALLOW_NAME_COLLISION=0
NPX_SRC="$HERE/skill/scripts/npx"
usage() {
  cat <<'EOF'
Usage: install.sh [--prefix DIR] [--root DIR] [--verify] [--with-npx-shim]

Build and install skillherd and its helpers (requires Cargo and a C linker).
  --prefix DIR      Install under DIR/bin (default: $HOME/.local).
  --root DIR        Use DIR/.local as the default prefix; --prefix wins.
  --verify          Report drift without installing (still builds to compare).
  --with-npx-shim   Keep the optional npx wrapper; otherwise remove owned copies.
  --allow-name-collision
                    Allow a foreign skillherd elsewhere on PATH (compatibility).
  -h, --help        Print this usage.

The wrapper redirects supported skills mutations through the managed importer.
Without it, use skillherd npx <args> explicitly. Reinstalling without the flag removes
an owned npx wrapper, including the old default wrapper. Foreign files stay put.
Upgrades remove installer-owned copies of the old skill command, never foreign ones.
Build artifacts remain in Cargo's target directory for faster repeat installs.
To reclaim the cache: cargo clean --manifest-path skill/Cargo.toml
EOF
}
while [ $# -gt 0 ]; do
  case "$1" in
    --root) [ $# -ge 2 ] || { echo "install.sh: --root needs a directory" >&2; exit 2; }; ROOT="$2"; shift ;;
    --prefix)
      [ $# -ge 2 ] || { echo "install.sh: --prefix needs a directory" >&2; exit 2; }
      [ -n "$2" ] || { echo "install.sh: --prefix needs a non-empty directory" >&2; exit 2; }
      PREFIX="$2"; shift ;;
    --verify) VERIFY=1 ;;
    --with-npx-shim) WITH_NPX=1 ;;
    --allow-name-collision) ALLOW_NAME_COLLISION=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done
PREFIX=${PREFIX:-$ROOT/.local}

# A content receipt recognizes an earlier publication without executing the
# destination (in particular, never probe procps' process-signalling skill).
# cksum is POSIX; the receipt is an accidental-overwrite guard, not protection
# against someone who can already rewrite this user's installation directory.
owned_tool() {
  local candidate="$1" src="$2" name="$3" receipt base family saw_receipt=0
  [ -f "$candidate" ] && [ ! -L "$candidate" ] || return 1
  if [ -f "$src" ] && cmp -s "$src" "$candidate"; then return 0; fi
  base="$(dirname "$(dirname "$candidate")")"
  for family in skillherd skill-cli; do
    receipt="$base/share/$family/installed-$name.cksum"
    if [ -e "$receipt" ] || [ -L "$receipt" ]; then
      [ ! -L "$base/share" ] && [ ! -L "$base/share/$family" ] \
        && [ -f "$receipt" ] && [ ! -L "$receipt" ] || return 1
      saw_receipt=1
      [ "$(cksum < "$candidate")" = "$(cat "$receipt")" ] && return 0
    fi
  done
  # A receipt mismatch is a local edit, not a legacy installation.
  # Known legacy npx bytes may replace a receipt-backed current shim.
  [ "$saw_receipt" -eq 0 ] || [ "$name" = npx ] || return 1
  case "$name" in
    # Old skill executables require receipts; a help phrase is not deletion proof.
    skill) return 1 ;;
    # Exact versioned self markers and frozen receipt-less release checksums.
    npx) if [ "$saw_receipt" -eq 0 ] && LC_ALL=C grep -a -x -q -e '# skillherd-npx-shim: 1' -e '# skill-cli-npx-shim: 1' "$candidate"; then return 0; fi
      case "$(cksum < "$candidate")" in '3516060573 3627'|'2488732261 4495') return 0 ;; *) return 1 ;; esac ;;
    skill-render) [ "$(cksum < "$candidate")" = '2222305786 28609' ] ;;
    *) return 1 ;;
  esac
}
check_tool() {
  local name="$1" src="$2" dst="$PREFIX/bin/$1" found remaining dir
  [ "$VERIFY" -eq 0 ] || return 0
  if [ -e "$dst" ] || [ -L "$dst" ]; then
    owned_tool "$dst" "$src" "$name" || {
      echo "install.sh: refusing to overwrite foreign $name: $dst (move it manually or choose another prefix)" >&2
      return 1
    }
  fi
  remaining="$PATH"
  while :; do
    dir="${remaining%%:*}"
    found="${dir:-.}/$name"
    if [ -f "$found" ] && [ -x "$found" ] && ! owned_tool "$found" "$src" "$name"; then
      if [ "$ALLOW_NAME_COLLISION" -eq 0 ]; then
        echo "install.sh: refusing name collision with foreign $name on PATH: $found (use --allow-name-collision to accept the PATH risk)" >&2
        return 1
      fi
      echo "WARNING: allowing foreign $name on PATH: $found" >&2
    fi
    case "$remaining" in *:*) remaining="${remaining#*:}" ;; *) break ;; esac
  done
}

check_receipts() {
  [ "$VERIFY" -eq 0 ] || return 0
  local item
  for item in "$PREFIX/share" "$PREFIX/share/skillherd" \
    "$PREFIX/share/skillherd/installed-skillherd.cksum" \
    "$PREFIX/share/skillherd/installed-skill-render.cksum" \
    "$PREFIX/share/skillherd/installed-npx.cksum"; do
    if [ -L "$item" ]; then
      echo "install.sh: refusing symlinked receipt path: $item" >&2
      return 1
    fi
  done
}
save_receipt() {
  local dst="$1" tmp
  check_receipts
  [ "$VERIFY" -eq 0 ] || return 0
  case "$dst" in
    "$PREFIX/bin/skillherd"|"$PREFIX/bin/skill-render"|"$PREFIX/bin/npx")
      mkdir -p "$PREFIX/share/skillherd"
      tmp=$(mktemp "$PREFIX/share/skillherd/.receipt.XXXXXX")
      cksum < "$dst" > "$tmp"
      mv -f "$tmp" "$PREFIX/share/skillherd/installed-${dst##*/}.cksum" ;;
  esac
}

check_npx_destination() {
  local dst="$PREFIX/bin/npx"
  [ "$WITH_NPX" -eq 1 ] && [ "$VERIFY" -eq 0 ] || return 0
  if [ ! -e "$dst" ] && [ ! -L "$dst" ]; then return 0; fi
  owned_tool "$dst" "$NPX_SRC" npx && return 0
  echo "install.sh: refusing to overwrite foreign npx: $dst" >&2
  return 1
}

# Removal uses the same content proof as replacement, rechecked at unlink.
remove_owned() {
  local name="$1" src="$2" reason="$3" dst="$PREFIX/bin/$1"
  owned_tool "$dst" "$src" "$name" || return 0
  if [ "$VERIFY" -eq 1 ]; then
    echo "UNWANTED $dst ($reason)"
    drift=1
    return 0
  fi
  owned_tool "$dst" "$src" "$name" || return 0
  rm -f "$dst"
  echo "removed $dst ($reason)"
}

drift=0
put() {  # put SRC DST
  local src="$1" dst="$2"
  # Recheck at publication too, after the build and any earlier copies.
  case "$dst" in
    "$PREFIX/bin/npx") check_npx_destination ;;
    "$PREFIX/bin/skillherd"|"$PREFIX/bin/skill-render") check_tool "${dst##*/}" "$src" ;;
  esac
  if [ -f "$dst" ] && [ ! -L "$dst" ] && cmp -s "$src" "$dst"; then
    save_receipt "$dst"
    echo "same $dst"
    return 0
  fi
  if [ "$VERIFY" -eq 1 ]; then
    if [ -e "$dst" ] || [ -L "$dst" ]; then echo "DIFF $dst"; else echo "MISSING $dst"; fi
    drift=1
    return 0
  fi
  mkdir -p "$(dirname "$dst")"
  [ -L "$dst" ] && rm -f "$dst"
  install -m 0755 "$src" "$dst"
  save_receipt "$dst"
  echo "put  $dst"
}

# The `skillherd` crate. The active developer directory on a Mac can refuse to
# link while its licence is unagreed; the Command Line Tools toolchain is the
# supported fallback (same probe as tests/skill-gate.sh).
build_skill() {
  command -v cargo >/dev/null 2>&1 || { echo "install.sh: cargo not found; the skillherd binary cannot be built" >&2; exit 1; }
  local probe_dir
  probe_dir=$(mktemp -d "${TMPDIR:-/tmp}/skillherd-install-probe.XXXXXX")
  printf 'int main(void) { return 0; }\n' > "$probe_dir/probe.c"
  if ! cc "$probe_dir/probe.c" -o "$probe_dir/probe" >/dev/null 2>&1; then
    if [ -d /Library/Developer/CommandLineTools ]; then
      export DEVELOPER_DIR=/Library/Developer/CommandLineTools
      echo "note: the active developer dir cannot link; using DEVELOPER_DIR=$DEVELOPER_DIR"
    else
      echo "install.sh: no working C toolchain; a linker is required to build the skillherd binary" >&2
      rm -rf "$probe_dir"
      exit 1
    fi
  fi
  rm -rf "$probe_dir"
  local manifest="$HERE/skill/Cargo.toml" target_dir
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HERE/skill/target}"
  nice -n 19 cargo build --release --locked --manifest-path "$manifest" \
    || { echo "install.sh: cargo build failed for $manifest" >&2; exit 1; }
  # The build and comparison agree without a JSON parser or Python. Set this
  # explicitly too, so Cargo config cannot send the artifact elsewhere.
  target_dir="$CARGO_TARGET_DIR"
  SKILLHERD_BUILT="$target_dir/release/skillherd"
  [ -x "$SKILLHERD_BUILT" ] || { echo "install.sh: built binary missing at $SKILLHERD_BUILT" >&2; exit 1; }
}

SKILLHERD_BUILT=""
check_npx_destination
build_skill
check_receipts
check_tool skillherd "$SKILLHERD_BUILT"
if [ -f "$HERE/bin/skill-render" ]; then check_tool skill-render "$HERE/bin/skill-render"; fi
put "$SKILLHERD_BUILT" "$PREFIX/bin/skillherd"
remove_owned skill '' 'renamed to skillherd'
if [ "$WITH_NPX" -eq 1 ]; then
  put "$NPX_SRC" "$PREFIX/bin/npx"
else
  remove_owned npx "$NPX_SRC" 'now opt-in; rerun with --with-npx-shim to keep it'
fi
if [ ! -f "$HERE/bin/skill-render" ]; then
  remove_owned skill-render '' 'retired; rendering is built into skillherd'
fi

PLATFORM=${SKILLHERD_CLI_PLATFORM:-$(uname -s | tr '[:upper:]' '[:lower:]')}
for f in "$HERE"/bin/*; do
  [ -f "$f" ] || continue
  if [ "${f##*/}" = npx ]; then continue; fi
  want=$(sed -n '2,20{s/^# platform: *\([a-z]*\).*/\1/p;}' "$f" | head -1)
  if [ -n "$want" ] && [ "$want" != "$PLATFORM" ]; then
    echo "skip $PREFIX/bin/$(basename "$f") ($want-only)"
    continue
  fi
  put "$f" "$PREFIX/bin/$(basename "$f")"
done
exit "$drift"
