#!/usr/bin/env bash
# Quality gate for the `skillherd` Rust crate: fmt, clippy, tests, release build.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
MANIFEST="$ROOT/skill/Cargo.toml"

echo '== tool versions =='
for tool in rustc cargo; do
  printf '%s: ' "$tool"
  "$tool" --version 2>&1 || echo 'MISSING'
done
for sub in fmt clippy; do
  printf 'cargo %s: ' "$sub"
  cargo "$sub" --version 2>&1 || echo 'MISSING'
done

# The active developer directory can refuse to link while its licence is
# unagreed (a fresh Xcode install or update resets it). The Command Line Tools
# toolchain is a supported fallback, so probe once and use it when the active
# one cannot link rather than failing the whole gate.
probe_dir=$(mktemp -d "${TMPDIR:-/tmp}/skillherd-gate-probe.XXXXXX")
printf 'int main(void) { return 0; }\n' > "$probe_dir/probe.c"
if ! cc "$probe_dir/probe.c" -o "$probe_dir/probe" >/dev/null 2>&1; then
  if [ -d /Library/Developer/CommandLineTools ]; then
    export DEVELOPER_DIR=/Library/Developer/CommandLineTools
    echo "note: the active developer dir cannot link; using DEVELOPER_DIR=$DEVELOPER_DIR"
  else
    echo 'error: no working C toolchain; a linker is required to build the crate'
    rm -rf "$probe_dir"
    exit 1
  fi
fi
rm -rf "$probe_dir"

status=0
run() {
  echo
  echo "== $* =="
  "$@" || status=1
}

run cargo fmt --manifest-path "$MANIFEST" --check
run cargo clippy --manifest-path "$MANIFEST" --all-targets -- -D warnings
run cargo test --manifest-path "$MANIFEST"
run cargo build --manifest-path "$MANIFEST" --release

echo
if [ "$status" -ne 0 ]; then
  echo 'GATE FAILED'
  exit 1
fi
echo 'GATE OK'
