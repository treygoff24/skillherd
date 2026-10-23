#!/usr/bin/env bash
# install.sh under a synthetic checkout with fixture tools and a stub cargo.
# These checks exercise publishing and verification, not Rust compilation.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
# Resolve the interpreter before the scratch HOME/PATH hide version-manager config.
TEST_PYTHON=$(python3 -c 'import sys; print(sys.executable)')
TMP=$(mktemp -d "${TMPDIR:-/tmp}/skillherd-install-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home-env"
mkdir -p "$HOME"

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

REPO="$TMP/repo"
mkdir -p "$REPO/bin"
cp "$ROOT/install.sh" "$REPO/install.sh"
mkdir -p "$TMP/stubs" "$REPO/skill" "$TMP/target/release"
printf '#!/usr/bin/env bash\nprintf "skillherd fixture\\n"\n' > "$TMP/target/release/skillherd"
chmod 0755 "$TMP/target/release/skillherd"
cat > "$TMP/stubs/cargo" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  build) printf '%s\n' "$@" > "$INSTALL_TEST_TMP/cargo-args"; printf 'visible build output\n' >&2; exit 0 ;;
  metadata) printf '{"target_directory":"%s/target"}\n' "$INSTALL_TEST_TMP" ;;
  *) exit 2 ;;
esac
EOF
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/stubs/cc"
chmod 0755 "$TMP/stubs/cargo" "$TMP/stubs/cc"
export INSTALL_TEST_TMP="$TMP"
export CARGO_TARGET_DIR="$TMP/target"
# Do not inspect or shadow tools installed on the owner's PATH.
mkdir -p "$TMP/tools"
for tool in basename bash cat chmod cksum cmp cp dirname env grep head install ln mkdir mktemp mv nice python3 readlink rm sed tail tr uname; do
  if [ "$tool" = bash ] && [ -n "${INSTALL_TEST_BASH:-}" ]; then
    ln -s "$INSTALL_TEST_BASH" "$TMP/tools/$tool"
  else
    ln -s "$(command -v "$tool")" "$TMP/tools/$tool"
  fi
done
export PATH="$TMP/stubs:$TMP/tools"
printf '#!/usr/bin/env bash\necho neutral\n' > "$REPO/bin/neutral"
printf '#!/usr/bin/env bash\n# platform: darwin\necho maconly\n' > "$REPO/bin/maconly"
chmod 0755 "$REPO/bin/neutral" "$REPO/bin/maconly"
INSTALL="$REPO/install.sh"

H="$TMP/home"
mkdir -p "$H/.local/bin"
# A stale symlink where a tool goes.
ln -s /nonexistent "$H/.local/bin/neutral"

SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$H" > "$TMP/one.out" 2>&1
[ $? -eq 0 ] && ok "install exits 0" || bad "install failed: $(cat "$TMP/one.out")"
for n in neutral maconly; do
  cmp -s "$REPO/bin/$n" "$H/.local/bin/$n" && [ ! -L "$H/.local/bin/$n" ] && [ -x "$H/.local/bin/$n" ] \
    && ok "bin/$n installed as a real executable" || bad "bin/$n not installed"
done
[ -x "$H/.local/bin/skillherd" ] && [ ! -L "$H/.local/bin/skillherd" ] \
  && ok "skillherd binary installed as a real executable" || bad "skillherd binary missing"
"$H/.local/bin/skillherd" --version > "$TMP/version.out" 2>&1 \
  && grep -q '^skillherd ' "$TMP/version.out" \
  && ok "installed skillherd runs (--version: $(cat "$TMP/version.out"))" || bad "installed skillherd does not run: $(cat "$TMP/version.out")"
grep -q "put  $H/.local/bin/skillherd" "$TMP/one.out" && ok "skillherd put reported" || bad "skillherd put not reported"

SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$H" > "$TMP/two.out" 2>&1
[ $? -eq 0 ] && ! grep -q '^put' "$TMP/two.out" && ok "second install is a no-op" || bad "second install re-put files: $(grep '^put' "$TMP/two.out")"

SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$H" --verify > "$TMP/v1.out" 2>&1
[ $? -eq 0 ] && ok "--verify clean after install" || bad "--verify reported drift: $(cat "$TMP/v1.out")"
printf '# drift\n' >> "$H/.local/bin/neutral"
printf '\n' >> "$H/.local/bin/skillherd"
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$H" --verify > "$TMP/v2.out" 2>&1
[ $? -eq 1 ] && grep -q "DIFF $H/.local/bin/neutral" "$TMP/v2.out" && grep -q "DIFF $H/.local/bin/skillherd" "$TMP/v2.out" \
  && ok "--verify names the drifted tool and the drifted binary and exits 1" || bad "--verify missed drift: $(cat "$TMP/v2.out")"
[ "$(tail -1 "$H/.local/bin/neutral")" = "# drift" ] && ok "--verify installed nothing" || bad "--verify changed a file"
# A modified executable no longer matches its receipt; restore it explicitly
# before asking the installer to repair the unguarded neutral fixture.
cp "$TMP/target/release/skillherd" "$H/.local/bin/skillherd"
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$H" > /dev/null 2>&1
cmp -s "$REPO/bin/neutral" "$H/.local/bin/neutral" && "$H/.local/bin/skillherd" --version > /dev/null 2>&1 \
  && ok "install repairs drift" || bad "drift not repaired"

# A darwin-only tool (header `# platform: darwin`) stays off a Linux host.
L="$TMP/linux"
mkdir -p "$L/.local/bin"
SKILLHERD_CLI_PLATFORM=linux "$INSTALL" --root "$L" > "$TMP/linux.out" 2>&1
[ $? -eq 0 ] && [ ! -e "$L/.local/bin/maconly" ] && grep -q 'skip .*maconly (darwin-only)' "$TMP/linux.out" \
  && ok "darwin-only tool skipped and reported on linux" || bad "darwin-only tool installed on linux: $(cat "$TMP/linux.out")"
[ -x "$L/.local/bin/neutral" ] && [ -x "$L/.local/bin/skillherd" ] && ok "platform-neutral tools and skillherd still install on linux" || bad "neutral tool or skillherd missing on linux"
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$L" > /dev/null 2>&1
[ -x "$L/.local/bin/maconly" ] && ok "darwin-only tool installs on darwin" || bad "darwin-only tool missing on darwin"

# Without cargo the install refuses instead of publishing a stale or absent binary.
B="$TMP/nocargo"
mkdir -p "$B/bin"
printf '#!/bin/sh\nexit 0\n' > "$B/bin/true"; chmod 0755 "$B/bin/true"
env -i PATH="$B/bin" HOME="$TMP" TMPDIR="$TMP" /bin/bash "$INSTALL" --root "$TMP/nocargo-home" > "$TMP/nocargo.out" 2>&1
[ $? -eq 1 ] && grep -q 'cargo not found' "$TMP/nocargo.out" && [ ! -e "$TMP/nocargo-home/.local/bin/skillherd" ] \
  && ok "missing cargo is an error and installs nothing" || bad "missing cargo not refused: $(cat "$TMP/nocargo.out")"

# Explicit prefixes are complete install roots, including when --root is used.
P="$TMP/custom prefix"
HOME="$TMP/default-home" SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --prefix "$P" > "$TMP/prefix.out" 2>&1
[ $? -eq 0 ] && [ -x "$P/bin/skillherd" ] && cmp -s "$REPO/bin/neutral" "$P/bin/neutral" \
  && [ ! -e "$TMP/default-home/.local" ] \
  && ok "--prefix installs binaries outside HOME, including paths with spaces" || bad "--prefix failed: $(cat "$TMP/prefix.out")"

for order in before after; do
  P="$TMP/prefix-$order"
  if [ "$order" = before ]; then
    SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --prefix "$P" --root "$TMP/root-$order" > "$TMP/order.out" 2>&1
  else
    SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --root "$TMP/root-$order" --prefix "$P" > "$TMP/order.out" 2>&1
  fi
  [ $? -eq 0 ] && [ -x "$P/bin/skillherd" ] && [ ! -e "$TMP/root-$order" ] \
    && ok "explicit prefix wins when --root is $order it" || bad "prefix/root order $order failed: $(cat "$TMP/order.out")"
done

P="$TMP/verify-prefix"
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --prefix "$P" --verify > "$TMP/prefix-missing.out" 2>&1
[ $? -eq 1 ] && [ ! -e "$P" ] && grep -Fq "MISSING $P/bin/skillherd" "$TMP/prefix-missing.out" \
  && ok "prefix --verify reports missing binaries without creating directories" || bad "prefix missing verify failed: $(cat "$TMP/prefix-missing.out")"

P="$TMP/custom prefix"
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --prefix "$P" --verify > "$TMP/prefix-clean.out" 2>&1
clean_rc=$?
if [ -f "$P/bin/neutral" ]; then printf '# prefix drift\n' >> "$P/bin/neutral"; fi
SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --prefix "$P" --verify > "$TMP/prefix-drift.out" 2>&1
drift_rc=$?
[ "$clean_rc" -eq 0 ] && [ "$drift_rc" -eq 1 ] && grep -Fq "DIFF $P/bin/neutral" "$TMP/prefix-drift.out" \
  && [ "$(tail -1 "$P/bin/neutral")" = '# prefix drift' ] \
  && ok "prefix --verify distinguishes clean and drifted files without repair" || bad "prefix drift verify failed: $(cat "$TMP/prefix-drift.out")"

"$INSTALL" --prefix > "$TMP/prefix-arg.out" 2>&1
[ $? -eq 2 ] && grep -q -- '--prefix needs a directory' "$TMP/prefix-arg.out" \
  && ok "--prefix requires an argument" || bad "missing prefix argument not diagnosed: $(cat "$TMP/prefix-arg.out")"
"$INSTALL" --prefix '' > "$TMP/prefix-empty.out" 2>&1
[ $? -eq 2 ] && grep -q -- '--prefix needs a non-empty directory' "$TMP/prefix-empty.out" \
  && ok "--prefix rejects an empty directory" || bad "empty prefix not refused: $(cat "$TMP/prefix-empty.out")"

# A custom prefix may be Node's own prefix. Refuse its npx before publishing
# even the skillherd binary, but keep updating our previously installed wrappers.
mkdir -p "$REPO/skill/scripts"
printf '#!/usr/bin/env bash\n# skillherd-npx-shim: 1\necho current\n' > "$REPO/skill/scripts/npx"
ln -s ../skill/scripts/npx "$REPO/bin/npx"
chmod 0755 "$REPO/bin/npx"
for kind in regular symlink; do
  P="$TMP/foreign-$kind"
  mkdir -p "$P/bin"
  printf '#!/bin/sh\necho node-npx\n' > "$TMP/node-$kind"
  if [ "$kind" = regular ]; then
    cp "$TMP/node-$kind" "$P/bin/npx"
  else
    ln -s "$TMP/node-$kind" "$P/bin/npx"
  fi
  SKILLHERD_CLI_PLATFORM=darwin "$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/foreign-$kind.out" 2>&1
  rc=$?
  preserved=0
  if [ "$kind" = regular ]; then
    [ ! -L "$P/bin/npx" ] && cmp -s "$TMP/node-$kind" "$P/bin/npx" && preserved=1
  else
    [ -L "$P/bin/npx" ] && [ "$(readlink "$P/bin/npx")" = "$TMP/node-$kind" ] && preserved=1
  fi
  [ "$rc" -eq 1 ] && [ "$preserved" -eq 1 ] \
    && grep -Fq "refusing to overwrite foreign npx: $P/bin/npx" "$TMP/foreign-$kind.out" \
    && [ ! -e "$P/bin/skillherd" ] && [ ! -e "$P/bin/neutral" ] \
    && ok "foreign $kind npx preserved with no partial publish" \
    || bad "foreign $kind npx not safely refused: $(cat "$TMP/foreign-$kind.out")"
done

# The versioned self marker proves ownership even without a receipt.
for action in update remove; do
  P="$TMP/marker-$action"
  mkdir -p "$P/bin"
  printf '#!/bin/sh\n# skillherd-npx-shim: 1\necho previous\n' > "$P/bin/npx"
  cmp -s "$P/bin/npx" "$REPO/bin/npx" && bad "marker fixture must differ from source"
  if [ "$action" = update ]; then
    "$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/marker.out" 2>&1
    [ $? -eq 0 ] && cmp -s "$P/bin/npx" "$REPO/bin/npx" \
      && ok "marker permits owned shim upgrade" || bad "marker upgrade failed"
  else
    "$INSTALL" --prefix "$P" > "$TMP/marker.out" 2>&1
    [ $? -eq 0 ] && [ ! -e "$P/bin/npx" ] \
      && ok "marker permits owned shim removal" || bad "marker removal failed"
  fi
done
# A look-alike marker from another tool is not ours.
P="$TMP/marker-foreign"
mkdir -p "$P/bin"
printf '#!/bin/sh\n# other-npx-shim: 1\necho other\n' > "$P/bin/npx"
cp "$P/bin/npx" "$TMP/marker-foreign-copy"
"$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/marker.out" 2>&1
[ $? -eq 1 ] && cmp -s "$P/bin/npx" "$TMP/marker-foreign-copy" \
  && ok "another tool's marker does not establish ownership" || bad "foreign marker allowed overwrite"
P="$TMP/modified-marked-shim"
"$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/modified-shim.out" 2>&1
printf '# local edit\n' >> "$P/bin/npx"
cp "$P/bin/npx" "$TMP/modified-marked-shim-copy"
"$INSTALL" --prefix "$P" >> "$TMP/modified-shim.out" 2>&1
[ $? -eq 0 ] && cmp -s "$P/bin/npx" "$TMP/modified-marked-shim-copy" \
  && ok "marker cannot override mismatched receipt for locally edited shim" || bad "locally edited marked shim removed"

# Installer regressions: opting in is required, builds are reproducible and
# visible, and publication cannot replace or shadow unrelated commands.
P="$TMP/default-no-shim"
"$INSTALL" --prefix "$P" > "$TMP/default.out" 2>&1
[ $? -eq 0 ] && [ ! -e "$P/bin/npx" ] && [ -x "$P/bin/skillherd" ] \
  && ok "npx is not installed by default" || bad "default installed npx or failed"
grep -qx -- '--locked' "$TMP/cargo-args" && ! grep -qx -- '--quiet' "$TMP/cargo-args" \
  && grep -q 'visible build output' "$TMP/default.out" \
  && ok "build is locked and output is visible" || bad "build not locked or hidden"
"$INSTALL" --help > "$TMP/help.out"
grep -q '^Usage:' "$TMP/help.out" && grep -q -- '--with-npx-shim' "$TMP/help.out" \
  && grep -q 'cargo clean' "$TMP/help.out" && ! grep -q '0755\|mid-rebase' "$TMP/help.out" \
  && ok "help shows usage and build-cache cleanup, not internal comments" || bad "help is not user-facing"

name=skillherd
  for kind in regular symlink path; do
    P="$TMP/foreign-$name-$kind"
    mkdir -p "$P/bin" "$TMP/path-$name"
    printf '#!/bin/sh\nprintf foreign\\n\n' > "$TMP/foreign-$name"
    chmod 0755 "$TMP/foreign-$name"
    if [ "$kind" = path ]; then
      cp "$TMP/foreign-$name" "$TMP/path-$name/$name"
    elif [ "$kind" = symlink ]; then
      ln -s "$TMP/foreign-$name" "$P/bin/$name"
    else
      cp "$TMP/foreign-$name" "$P/bin/$name"
    fi
    PATH="$TMP/path-$name:$PATH" "$INSTALL" --prefix "$P" > "$TMP/guard.out" 2>&1
    rc=$?
    if [ "$kind" = path ]; then candidate="$TMP/path-$name/$name"; else candidate="$P/bin/$name"; fi
    [ "$rc" -eq 1 ] && cmp -s "$TMP/foreign-$name" "$candidate" \
      && grep -q "refusing.*foreign $name" "$TMP/guard.out" && [ ! -e "$P/bin/neutral" ] \
      && ok "foreign $kind $name is preserved before publication" || bad "foreign $kind $name guard failed: $(cat "$TMP/guard.out")"
    if [ "$kind" = path ]; then rm -f "$candidate"; fi
  done

P="$TMP/owned-upgrade"
"$INSTALL" --prefix "$P" > "$TMP/upgrade.out" 2>&1
# A changed build is recognized by the receipt for the previous bytes.
printf '# upgraded build\n' >> "$TMP/target/release/skillherd"
"$INSTALL" --prefix "$P" >> "$TMP/upgrade.out" 2>&1
[ $? -eq 0 ] && cmp -s "$TMP/target/release/skillherd" "$P/bin/skillherd" \
  && ok "receipts allow owned binary upgrades" || bad "owned upgrade refused: $(cat "$TMP/upgrade.out")"
printf '# user modification\n' >> "$P/bin/skillherd"
cp "$P/bin/skillherd" "$TMP/modified-skill"
"$INSTALL" --prefix "$P" > "$TMP/modified.out" 2>&1
[ $? -eq 1 ] && cmp -s "$TMP/modified-skill" "$P/bin/skillherd" \
  && ok "modified binary is preserved instead of assumed owned" || bad "modified binary was overwritten"

for kind in directory file; do
  P="$TMP/receipt-$kind"
  mkdir -p "$P/share" "$TMP/receipt-outside-$kind"
  printf 'outside receipt data\n' > "$TMP/receipt-outside-$kind/installed-skillherd.cksum"
  if [ "$kind" = directory ]; then
    ln -s "$TMP/receipt-outside-$kind" "$P/share/skillherd"
  else
    mkdir -p "$P/share/skillherd"
    ln -s "$TMP/receipt-outside-$kind/installed-skillherd.cksum" "$P/share/skillherd/installed-skillherd.cksum"
  fi
  "$INSTALL" --prefix "$P" > "$TMP/receipt.out" 2>&1
  [ $? -eq 1 ] && [ ! -e "$P/bin/skillherd" ] \
    && [ "$(cat "$TMP/receipt-outside-$kind/installed-skillherd.cksum")" = 'outside receipt data' ] \
    && grep -q 'symlinked receipt' "$TMP/receipt.out" \
    && ok "symlinked receipt $kind is refused before publication" || bad "receipt $kind redirected a write"
done

# Exercise the real shim with spies. Every expectation checks exact argv,
# ensuring a pass-through cannot accidentally count as a routed command.
mkdir -p "$TMP/real-first" "$TMP/real-second"
cat > "$TMP/real-first/npx" <<'EOF'
#!/bin/bash
printf 'raw\n'
printf '<%s>\n' "$@"
EOF
cat > "$TMP/skill-spy" <<'EOF'
#!/bin/bash
printf 'managed\n'
printf '<%s>\n' "$@"
EOF
printf '#!/bin/bash\nprintf second\\n\n' > "$TMP/real-second/npx"
chmod 0755 "$TMP/real-first/npx" "$TMP/real-second/npx" "$TMP/skill-spy"
shim_check() {
  local label="$1" expected="$2"; shift 2
  SKILLHERD_BIN="$TMP/skill-spy" SKILLHERD_REAL_NPX="$TMP/real-first/npx" \
    "$ROOT/bin/npx" "$@" > "$TMP/shim.out" 2> "$TMP/shim.err"
  [ $? -eq 0 ] && [ "$(cat "$TMP/shim.out")" = "$expected" ] \
    && ok "$label" || bad "$label: $(cat "$TMP/shim.out" "$TMP/shim.err")"
}
for package in some-pkg agent-browser; do
  shim_check "only package position routes ($package)" "$(printf 'raw\n<%s>\n<skills>\n<add>\n<example/repo>' "$package")" "$package" skills add example/repo
 done
shim_check "another package's -- boundary is untouched" $'raw\n<some-pkg>\n<-->\n<skills>\n<add>\n<repo>' some-pkg -- skills add repo
shim_check "npx -- introduces the package" $'managed\n<install>\n<repo>' -y -- skills add repo
shim_check "skills find is read-only pass-through" $'raw\n<skills>\n<find>\n<a>' skills find a
shim_check "read-only explicit version is preserved" $'raw\n<skills@9.9.9>\n<find>\n<a>' skills@9.9.9 find a
shim_check "npx value option keeps package position" $'managed\n<install>\n<repo>' --cache '/scratch cache' skills add repo
SKILLHERD_REAL_NPX="$TMP/real-first/npx" "$ROOT/bin/npx" --unknown-prefix skills add repo > "$TMP/shim.out" 2> "$TMP/shim.err"
[ $? -eq 2 ] && [ ! -s "$TMP/shim.out" ] && grep -q 'unsupported npx option' "$TMP/shim.err" \
  && ok "ambiguous npx options cannot bypass managed mutation" || bad "unknown npx option escaped to raw npx"
shim_check "pinned version routes" $'managed\n<install>\n<repo>' skills@1.7.0 add repo
shim_check "mutation -- preserves literal option-looking names" $'managed\n<remove>\n<-->\n<-y>' skills remove -- -y
for verb in experimental_sync experimental_install; do
  SKILLHERD_REAL_NPX="$TMP/real-first/npx" "$ROOT/bin/npx" skills "$verb" > "$TMP/shim.out" 2> "$TMP/shim.err"
  [ $? -eq 2 ] && [ ! -s "$TMP/shim.out" ] && grep -q 'unsupported skills command' "$TMP/shim.err" \
    && ok "$verb cannot run against live HOME" || bad "$verb escaped to raw npx"
done
SKILLHERD_BIN="$TMP/skill-spy" SKILLHERD_REAL_NPX="$TMP/real-first/npx" "$ROOT/bin/npx" skills@9.9.9 add repo > "$TMP/shim.out" 2> "$TMP/shim.err"
[ $? -eq 2 ] && [ ! -s "$TMP/shim.out" ] && grep -q 'version.*skills@1.7.0' "$TMP/shim.err" \
  && ok "unsupported mutation version is refused explicitly" || bad "explicit package version was dropped"
PATH="$TMP/real-first:$TMP/real-second:$PATH" SKILLHERD_REAL_NPX='' "$ROOT/bin/npx" --version > "$TMP/path-order.out" 2>&1
[ $? -eq 0 ] && [ "$(cat "$TMP/path-order.out")" = $'raw\n<--version>' ] \
  && ok "real npx follows PATH order" || bad "PATH order ignored: $(cat "$TMP/path-order.out")"

# Desired-state upgrades remove only owned optional commands.
for ownership in identical receipt; do
  P="$TMP/remove-$ownership"
  "$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/remove.out" 2>&1
  if [ "$ownership" = receipt ]; then printf '# new shim\n' >> "$REPO/skill/scripts/npx"; fi
  "$INSTALL" --verify --prefix "$P" > "$TMP/remove-verify.out" 2>&1
  verify_rc=$?
  [ -f "$P/bin/npx" ] || bad "removal fixture missing ($ownership)"
  "$INSTALL" --prefix "$P" >> "$TMP/remove.out" 2>&1
  [ $? -eq 0 ] && [ "$verify_rc" -eq 1 ] && [ ! -e "$P/bin/npx" ] \
    && grep -q 'UNWANTED.*npx' "$TMP/remove-verify.out" && grep -q 'not requested' "$TMP/remove.out" \
    && ok "owned $ownership shim removed unless opted in; verify reports it" || bad "owned $ownership shim remains: $(cat "$TMP/remove.out" "$TMP/remove-verify.out")"
done
P="$TMP/foreign-unwanted"
mkdir -p "$P/bin"
printf '#!/bin/sh\n# thin wrapper around the real npx\necho user\n' > "$P/bin/npx"
cp "$P/bin/npx" "$TMP/user-npx"
"$INSTALL" --prefix "$P" > "$TMP/user.out" 2>&1
[ $? -eq 0 ] && cmp -s "$TMP/user-npx" "$P/bin/npx" \
  && ok "default leaves foreign wrapper alone" || bad "foreign unwanted npx touched"
"$INSTALL" --with-npx-shim --prefix "$P" > "$TMP/user.out" 2>&1
[ $? -eq 1 ] && cmp -s "$TMP/user-npx" "$P/bin/npx" \
  && ok "generic wrapper phrase does not establish ownership" || bad "generic phrase allowed overwrite"

# Every PATH hit matters even when an owned copy precedes a foreign one.
P="$TMP/path-all"
"$INSTALL" --prefix "$P" > "$TMP/path-all.out" 2>&1
mkdir -p "$TMP/late-system"
printf '#!/bin/sh\necho MUST_NOT_EXECUTE\nexit 93\n' > "$TMP/late-system/skillherd"
chmod 0755 "$TMP/late-system/skillherd"
PATH="$P/bin:$PATH:$TMP/late-system" "$INSTALL" --prefix "$P" >> "$TMP/path-all.out" 2>&1
[ $? -eq 1 ] && grep -q "foreign skillherd.*$TMP/late-system/skillherd" "$TMP/path-all.out" \
  && ! grep -q MUST_NOT_EXECUTE "$TMP/path-all.out" \
  && ok "foreign later PATH hit refused even behind owned skillherd" || bad "later PATH collision missed"
PATH="$P/bin:$PATH:$TMP/late-system" "$INSTALL" --allow-name-collision --prefix "$P" > "$TMP/override.out" 2>&1
[ $? -eq 0 ] && grep -q 'WARNING: allowing foreign skillherd' "$TMP/override.out" \
  && ok "explicit collision override proceeds with a warning" || bad "collision override failed: $(cat "$TMP/override.out")"

P="$TMP/hardlink-receipt"
"$INSTALL" --prefix "$P" > "$TMP/hardlink.out" 2>&1
cp "$P/share/skillherd/installed-skillherd.cksum" "$TMP/receipt-copy"
ln "$P/share/skillherd/installed-skillherd.cksum" "$TMP/receipt-hardlink"
printf '# another build\n' >> "$TMP/target/release/skillherd"
"$INSTALL" --prefix "$P" >> "$TMP/hardlink.out" 2>&1
[ $? -eq 0 ] && cmp -s "$TMP/receipt-copy" "$TMP/receipt-hardlink" \
  && ! cmp -s "$TMP/receipt-hardlink" "$P/share/skillherd/installed-skillherd.cksum" \
  && ok "receipt replacement does not write through hardlinks" || bad "receipt wrote through hardlink"

P="$TMP/no-python"
rm "$TMP/tools/python3"
"$INSTALL" --prefix "$P" > "$TMP/no-python.out" 2>&1
[ $? -eq 0 ] && [ -x "$P/bin/skillherd" ] \
  && ok "installer needs no Python" || bad "Python still required: $(cat "$TMP/no-python.out")"

# Cargo's default target directory is relative to the crate, not the caller.
mkdir -p "$REPO/skill/target/release"
cp "$TMP/target/release/skillherd" "$REPO/skill/target/release/skillherd"
printf '# default target\n' >> "$REPO/skill/target/release/skillherd"
env -u CARGO_TARGET_DIR "$INSTALL" --prefix "$TMP/default-target" > "$TMP/default-target.out" 2>&1
[ $? -eq 0 ] && cmp -s "$REPO/skill/target/release/skillherd" "$TMP/default-target/bin/skillherd" \
  && ok "default artifact comes from the crate target directory" || bad "default target resolution failed: $(cat "$TMP/default-target.out")"

# A phrase in someone else's wrapper is not a self marker.
mkdir -p "$TMP/user-wrapper"
printf '#!/bin/sh\n# thin wrapper around the real npx\nprintf user-npx\n' > "$TMP/user-wrapper/npx"
chmod 0755 "$TMP/user-wrapper/npx"
PATH="$TMP/user-wrapper:$PATH" SKILLHERD_REAL_NPX='' "$ROOT/bin/npx" --version > "$TMP/user-wrapper.out" 2>&1
[ $? -eq 0 ] && [ "$(cat "$TMP/user-wrapper.out")" = user-npx ] \
  && ok "generic wrapper phrase is a usable real npx" || bad "user wrapper incorrectly skipped"
# Timeout bounds the infinite exec loop if the self-skip is removed.
mkdir -p "$TMP/current-shim"
cp "$ROOT/bin/npx" "$TMP/current-shim/npx"
chmod 0755 "$TMP/current-shim/npx"
PATH="$TMP/current-shim:$TMP/real-first:$PATH" SKILLHERD_REAL_NPX='' \
  "$TEST_PYTHON" -c 'import subprocess, sys
try:
    sys.exit(subprocess.run(sys.argv[1:], timeout=3).returncode)
except subprocess.TimeoutExpired:
    sys.exit(124)
' "$ROOT/bin/npx" --version > "$TMP/self-skip.out" 2>&1
self_skip_rc=$?
[ "$self_skip_rc" -eq 0 ] && [ "$(cat "$TMP/self-skip.out")" = $'raw\n<--version>' ] \
  && ok "wrapper copy on PATH skipped under timeout" \
  || bad "wrapper self-skip failed (exit $self_skip_rc): $(cat "$TMP/self-skip.out")"
SKILLHERD_NPX_PKG=skills@2.0.0 shim_check "configured pin routes" $'managed\n<install>\n<repo>' skills@2.0.0 add repo
skill_prefix=INJECTED shim_check "environment cannot inject managed prefix" $'managed\n<install>\n<repo>' skills add repo
SKILLHERD_BIN="$TMP/skill-spy" SKILLHERD_REAL_NPX="$TMP/real-first/npx" "$ROOT/bin/npx" --dry-run skills add repo > "$TMP/npm-dry.out" 2> "$TMP/npm-dry.err"
[ $? -eq 2 ] && [ ! -s "$TMP/npm-dry.out" ] && grep -q 'before npx' "$TMP/npm-dry.err" \
  && ok "npm dry-run cannot become a real managed mutation" || bad "npm dry-run was dropped for skills"
SKILLHERD_REAL_NPX="$TMP/real-first/npx" "$ROOT/bin/npx" -p foo skills add repo > "$TMP/package.out" 2> "$TMP/package.err"
[ $? -eq 2 ] && [ ! -s "$TMP/package.out" ] && grep -q 'package.*unsupported' "$TMP/package.err" \
  && ok "-p foo cannot hide a skills command" || bad "-p foo bypassed routing refusal"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
