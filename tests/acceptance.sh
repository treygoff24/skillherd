#!/usr/bin/env bash
# tests/acceptance.sh: end-to-end checks of the release binary in a scratch HOME.
#
#   1. tests/skill-gate.sh green (SKIP_GATE=1 skips it while iterating on 2-3).
#   2. Fixture end-to-end: four library skills (plain; mac/linux fences; a
#      claude-only fence; a codex-only row), a `sets` file, an upstream skill
#      imported through `skillherd install` and the stub npx from a local bare repo
#      carrying a Claude variant and Claude agents, and a foreign real dir on
#      the Claude surface. `skillherd on`, then `skillherd render` twice: the second
#      render is all-unchanged, Claude serves the variant, the shared dir serves
#      the lane rows (universal copy, no `only:` rows), the foreign dir is
#      byte-identical, `skillherd doctor` exits 0.
#   3. A second `skillherd install` through the stub npx: the child ran with
#      HOME=stage, and the scratch HOME is untouched except agents_dir, the lock
#      and the library.
#
# Nothing here touches the caller's HOME: every `skill` call gets
# HOME=<scratch>. Git identity comes only from the GIT_AUTHOR_*/GIT_COMMITTER_*
# variables set below: the scratch HOME's git config forbids guessing one, so
# macOS behaves like a Linux host with no configured identity.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="$ROOT/skill/target/release/skillherd"
FIX="$ROOT/skill/tests/fixtures"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/skillherd-acceptance.XXXXXX")
TMP=$(cd "$TMP" && pwd)  # macOS TMPDIR ends in a slash; keep every path we compare single-slashed
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

# --- 1. the gate -------------------------------------------------------------
if [ "${SKIP_GATE:-0}" = 1 ]; then
  printf 'skip gate (SKIP_GATE=1)\n'
else
  if bash "$ROOT/tests/skill-gate.sh" > "$TMP/gate.log" 2>&1; then
    ok "tests/skill-gate.sh GATE OK"
  else
    bad "tests/skill-gate.sh failed: $(grep -E 'FAILED|error' "$TMP/gate.log" | head -3)"
  fi
fi
[ -x "$BIN" ] || { bad "release binary missing at $BIN"; printf '%d passed, %d failed\n' "$pass" "$fail"; exit 1; }

# --- helpers -----------------------------------------------------------------
H="$TMP/home"
LIB="$H/.agents/skill-library"
SHARED="$H/.agents/skills"
LOCK="$H/.agents/.skill-lock.json"
CLAUDE="$H/.claude/skills"
CODEX="$H/.codex/skills"
mkdir -p "$LIB/.globals" "$SHARED" "$CLAUDE" "$H/.codex" "$H/.pi/agent/skills" "$H/.cursor/skills" "$H/bin"

# A fixed identity for every commit this run makes, and a scratch git config
# that refuses to invent one (git on macOS guesses from the host name; on Linux
# a missing identity fails the commit).
export GIT_AUTHOR_NAME=acceptance GIT_AUTHOR_EMAIL=acceptance@example.invalid
export GIT_COMMITTER_NAME=acceptance GIT_COMMITTER_EMAIL=acceptance@example.invalid
printf '[user]\n\tuseConfigOnly = true\n' > "$H/.gitconfig"

# Every skillherd call: scratch HOME, the stub npx, the identity.
sk() {
  env -i PATH="$PATH" HOME="$H" SKILLHERD_NPX="$H/bin/npx" \
    SKILLHERD_PLATFORM=mac \
    GIT_AUTHOR_NAME="$GIT_AUTHOR_NAME" GIT_AUTHOR_EMAIL="$GIT_AUTHOR_EMAIL" \
    GIT_COMMITTER_NAME="$GIT_COMMITTER_NAME" GIT_COMMITTER_EMAIL="$GIT_COMMITTER_EMAIL" \
    "$BIN" "$@"
}
jq_of() {  # jq_of <json-file> <python-expr over d>
  python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$1" "$2"
}
tree_hash() {  # content + names + link targets of a tree
  (cd "$1" && find . -print0 | sort -z | while IFS= read -r -d '' p; do
     if [ -L "$p" ]; then printf 'L %s -> %s\n' "$p" "$(readlink "$p")"
     elif [ -f "$p" ]; then printf 'F %s %s\n' "$p" "$(shasum -a 256 < "$p" | cut -c1-64)"
     else printf 'D %s\n' "$p"; fi
   done) | shasum -a 256 | cut -c1-64
}
skill_md() {  # skill_md <dir> <name> <body>
  mkdir -p "$1/$2"
  printf -- '---\nname: %s\ndescription: %s fixture\n---\n\n%s\n' "$2" "$2" "$3" > "$1/$2/SKILL.md"
}
git_lib() { git -C "$LIB" -c user.name=fixture -c user.email=fixture@example.invalid "$@"; }

# --- 2. fixture end-to-end -----------------------------------------------------
skill_md "$LIB" plain '# plain

NEUTRAL'
skill_md "$LIB" machine-fenced '# machine-fenced

<!-- mac-only -->
MAC-TEXT
<!-- /mac-only -->
<!-- linux-only -->
LINUX-TEXT
<!-- /linux-only -->
NEUTRAL'
skill_md "$LIB" claude-fenced '# claude-fenced

<!-- claude-only -->
CLAUDE-TEXT
<!-- /claude-only -->
NEUTRAL'
skill_md "$LIB" codexonly '# codexonly

CODEX'
cp "$ROOT/tests/fixtures/harnesses" "$LIB/.globals/harnesses"
cat > "$LIB/.globals/sets" <<'EOF'
# acceptance fixture
plain           main lane
machine-fenced  main
claude-fenced   main
codexonly       main only:codex
EOF
# The seeded skills were written here, not imported: list them in .globals/house.
printf '# acceptance fixture\nplain\nmachine-fenced\nclaude-fenced\ncodexonly\n' > "$LIB/.globals/house"
printf '# ignored\ninbound\n# other note\n' > "$LIB/.gitignore"
git_lib init -q && git_lib add -A && git_lib commit -q -m seed

# The upstream skill: a bare repo carrying a universal copy, a Claude variant
# and Claude agents, one named for the skill (imported) and one that is not
# (left upstream). `skillherd install` runs the stub npx in the stage, which copies
# the universal copy into the stage and writes a lock entry pointing at the
# bare repo; reconcile then imports the stage and clones the bare repo for the
# variant and the agents.
bash "$FIX/make-upstream.sh" init "$TMP/upstream-work" inbound "CLAUDE-VARIANT" \
  && mkdir -p "$TMP/upstream-work/.claude/agents" \
  && printf -- '---\nname: inbound-helper\n---\nINBOUND-AGENT\n' > "$TMP/upstream-work/.claude/agents/inbound-helper.md" \
  && printf -- '---\nname: stranger\n---\nSTRANGER\n' > "$TMP/upstream-work/.claude/agents/stranger.md" \
  && bash "$FIX/make-upstream.sh" publish "$TMP/upstream-work" "$TMP/inbound.git" \
  && ok "bare upstream built" || bad "make-upstream.sh failed"
cp "$FIX/npx-stub.sh" "$H/bin/npx" && chmod 0755 "$H/bin/npx"
cat > "$H/bin/npx.conf" <<EOF
log=$TMP/npx-inbound.log
source_dir=$TMP/upstream-work
source_url=file://$TMP/inbound.git
skill_path=.agents/skills/inbound/SKILL.md
EOF
# A foreign real dir on the Claude surface, hashed before anything runs.
skill_md "$CLAUDE" handmade '# handmade

HAND'
foreign_before=$(tree_hash "$CLAUDE/handmade")
# An agent another tool installed in Claude's agents dir.
AGENTS_SERVED="$H/.claude/agents"
mkdir -p "$AGENTS_SERVED" && printf 'OTHER-AGENT\n' > "$AGENTS_SERVED/worker.md"

sk install fixture/inbound > "$TMP/inbound.json" 2> "$TMP/inbound.err"
[ $? -eq 0 ] && [ "$(jq_of "$TMP/inbound.json" "d['ok']")" = True ] \
  && ok "skillherd install fixture/inbound ok" || bad "skillherd install failed: $(head -c 400 "$TMP/inbound.json") $(head -c 200 "$TMP/inbound.err")"
[ "$(jq_of "$TMP/inbound.json" "[i['name'] for i in d['data']['imported']]")" = "['inbound']" ] \
  && ok "install imported inbound" || bad "imported list: $(jq_of "$TMP/inbound.json" "d['data'].get('imported')")"
[ -f "$LIB/inbound/SKILL.md" ] && [ -f "$LIB/.npx/inbound.json" ] && ok "library holds inbound and its record" || bad "inbound not in the library"
[ -f "$LIB/inbound/.variants/claude/SKILL.md" ] && grep -q CLAUDE-VARIANT "$LIB/inbound/.variants/claude/SKILL.md" \
  && ok "claude variant imported from upstream" || bad "claude variant missing"
[ ! -e "$H/.agents/.skill-stage" ] && [ ! -e "$SHARED/inbound" ] && ok "stage removed, nothing left in the shared dir" || bad "stage or shared copy left behind: $(ls -a "$H/.agents" "$SHARED")"
[ -f "$LIB/inbound/.harness-agents/claude/inbound-helper.md" ] && [ ! -e "$LIB/inbound/.harness-agents/claude/stranger.md" ] \
  && [ "$(jq_of "$LIB/.npx/inbound.json" "sorted(d.get('harnessAgents', {}))")" = "['claude']" ] \
  && ok "the skill's own Claude agent imported and recorded, the stranger left upstream" || bad "agent import wrong: $(ls -R "$LIB/inbound/.harness-agents" 2>&1 | head -5)"
git_lib log --format=%s | grep -q '^npx: import inbound' && ok "import commit present" || bad "no import commit: $(git_lib log --oneline | head -3)"
grep -q '^inbound$' "$LIB/.gitignore" && bad ".gitignore still lists the imported name" || ok "imported name left .gitignore"
grep -q '^# other note$' "$LIB/.gitignore" && ok ".gitignore other lines intact" || bad ".gitignore lost a line"

# Subagents are served only for skills listed in `.globals/agents`.
printf 'inbound\n' > "$LIB/.globals/agents"
sk on inbound > "$TMP/on.json" 2>&1 && ok "skillherd on inbound" || bad "skillherd on failed: $(head -c 300 "$TMP/on.json")"
sk render > "$TMP/render1.json" 2>&1 && ok "first render ok" || bad "first render failed: $(head -c 400 "$TMP/render1.json")"
sk render > "$TMP/render2.json" 2>&1 && ok "second render ok" || bad "second render failed: $(head -c 400 "$TMP/render2.json")"
changed=$(jq_of "$TMP/render2.json" "sum(len(s['linked'])+len(s['removed'])+len(s['refreshed']) for s in d['data']['surfaces']) + sum(len(a['linked'])+len(a['removed']) for a in d['data']['agents'])")
[ "$changed" = 0 ] && ok "second render is all-unchanged" || bad "second render changed $changed entries: $(jq_of "$TMP/render2.json" "[(s['surface'],s['linked'],s['removed'],s['refreshed']) for s in d['data']['surfaces']]")"

[ -L "$CLAUDE/inbound" ] && case "$(readlink "$CLAUDE/inbound")" in *inbound/.variants/claude) ok "claude serves the inbound variant";; *) bad "claude link target: $(readlink "$CLAUDE/inbound")";; esac
[ -L "$SHARED/plain" ] && [ "$(readlink "$SHARED/plain")" = "../skill-library/plain" ] && ok "shared dir serves the lane row (universal, relative)" || bad "shared dir/plain: $(readlink "$SHARED/plain" 2>&1)"
[ ! -e "$SHARED/codexonly" ] && [ ! -e "$SHARED/machine-fenced" ] && [ ! -e "$SHARED/inbound" ] && ok "shared dir carries no main-only or only: rows" || bad "shared dir holds a non-lane entry: $(ls "$SHARED")"
[ -e "$CODEX/codexonly" ] && [ ! -e "$CLAUDE/codexonly" ] && ok "only:codex row on codex alone" || bad "only:codex placement wrong"
[ -d "$CLAUDE/machine-fenced" ] && [ ! -L "$CLAUDE/machine-fenced" ] && grep -q MAC-TEXT "$CLAUDE/machine-fenced/SKILL.md" && ! grep -q LINUX-TEXT "$CLAUDE/machine-fenced/SKILL.md" \
  && ok "machine fence stripped on the served copy (mac kept, linux dropped)" || bad "machine fence not rendered: $(ls -la "$CLAUDE" | head)"
grep -q CLAUDE-TEXT "$CLAUDE/claude-fenced/SKILL.md" && ! grep -q '<!--' "$CLAUDE/claude-fenced/SKILL.md" && ok "claude-only block kept on claude, markers gone" || bad "claude fence wrong on claude"
[ -e "$CODEX/claude-fenced/SKILL.md" ] && ! grep -q CLAUDE-TEXT "$CODEX/claude-fenced/SKILL.md" && ok "claude-only block stripped on codex" || bad "claude fence leaked to codex"
[ "$(tree_hash "$CLAUDE/handmade")" = "$foreign_before" ] && ok "foreign dir on the claude surface byte-identical" || bad "foreign dir changed"
[ -L "$AGENTS_SERVED/inbound-helper.md" ] && [ "$(readlink "$AGENTS_SERVED/inbound-helper.md")" = "../../.agents/skill-library/inbound/.harness-agents/claude/inbound-helper.md" ] \
  && grep -q INBOUND-AGENT "$AGENTS_SERVED/inbound-helper.md" && ok "claude agents dir serves the imported agent from the pool" || bad "agent link: $(ls -la "$AGENTS_SERVED" 2>&1)"
[ ! -L "$AGENTS_SERVED/worker.md" ] && [ "$(cat "$AGENTS_SERVED/worker.md")" = "OTHER-AGENT" ] && [ ! -e "$AGENTS_SERVED/stranger.md" ] \
  && ok "the other tool's agent untouched, no stray agent served" || bad "agents dir: $(ls -la "$AGENTS_SERVED")"

sk doctor > "$TMP/doctor.json" 2>&1; rc=$?
[ $rc -eq 0 ] && ok "skillherd doctor exits 0" || bad "doctor exit $rc: $(jq_of "$TMP/doctor.json" "[(f['code'],f['path']) for f in d['data']['findings'] if f['severity']!='info']" 2>/dev/null || head -c 300 "$TMP/doctor.json")"

# --- 3. install through the stub npx --------------------------------------------
mkdir -p "$TMP/stub-source/.agents/skills"
skill_md "$TMP/stub-source/.agents/skills" staged 'STAGED UNIVERSAL'
bash "$FIX/make-upstream.sh" init "$TMP/staged-work" staged "STAGED CLAUDE" \
  && bash "$FIX/make-upstream.sh" publish "$TMP/staged-work" "$TMP/staged.git"
cat > "$H/bin/npx.conf" <<EOF
log=$TMP/npx.log
source_dir=$TMP/stub-source
source_url=file://$TMP/staged.git
skill_path=.agents/skills/staged/SKILL.md
EOF
before_claude=$(tree_hash "$H/.claude"); before_codex=$(tree_hash "$H/.codex")
before_pi=$(tree_hash "$H/.pi"); before_cursor=$(tree_hash "$H/.cursor"); before_bin=$(tree_hash "$H/bin")

sk install owner/repo > "$TMP/install.json" 2> "$TMP/install.err"
[ $? -eq 0 ] && ok "skillherd install ok" || bad "install failed: $(head -c 400 "$TMP/install.json") $(head -c 300 "$TMP/install.err")"
command grep -qF -- "HOME=$H/.agents/.skill-stage" "$TMP/npx.log" && ok "npx ran with HOME=stage" || bad "npx HOME: $(grep '^HOME=' "$TMP/npx.log")"
grep -q -- '-a universal' "$TMP/npx.log" && ok "npx add targeted -a universal" || bad "npx argv: $(grep '^ARGV=' "$TMP/npx.log")"
[ -f "$LIB/staged/SKILL.md" ] && [ -f "$LIB/.npx/staged.json" ] && ok "install imported staged into the library" || bad "staged not imported"
[ ! -e "$H/.agents/.skill-stage" ] && ok "stage removed" || bad "stage left behind"
python3 -c 'import json,sys; sys.exit(0 if "staged" in json.load(open(sys.argv[1]))["skills"] else 1)' "$LOCK" && ok "live lock names staged" || bad "live lock missing staged"
[ "$(tree_hash "$H/.claude")" = "$before_claude" ] && [ "$(tree_hash "$H/.codex")" = "$before_codex" ] \
  && [ "$(tree_hash "$H/.pi")" = "$before_pi" ] && [ "$(tree_hash "$H/.cursor")" = "$before_cursor" ] && [ "$(tree_hash "$H/bin")" = "$before_bin" ] \
  && ok "scratch HOME untouched outside agents_dir, lock and library" || bad "install changed a surface it does not own"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
