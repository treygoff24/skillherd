#!/usr/bin/env bash
# Stub `npx` for the reconcile tests.
#
# It is copied to `<home>/bin/npx` (with `<home>/bin/npx.conf` beside it), so
# every test gets its own log and behaviour without touching the environment.
# It records the `HOME` it was given and its argv, then stands in for
# `npx -y skills@1.7.0`:
#
#   add <source> [--skill N]… -g -a universal -y   copy `.agents/skills/*` from
#                                                  `source_dir` into the stage
#                                                  and write a lock entry
#   update [name]… -g -y                           re-copy from `source_dir`
#   remove <name>… -g -y                           drop the dirs and the entries
#
# npx.conf keys: log, exit, source_dir, source_url, skill_path.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
conf="$here/npx.conf"

val() {
  [ -f "$conf" ] || return 0
  sed -n "s/^$1=//p" "$conf" | head -n1
}

log=$(val log)
if [ -n "$log" ]; then
  {
    echo "HOME=$HOME"
    echo "ARGV=$*"
  } >> "$log"
fi
# The harness-home overrides as this child sees them (`unset` when scrubbed),
# one per line, rewritten on every run, beside the log (the stub's own dir may
# be a surface a test asserts untouched).
if [ -n "$log" ]; then
  {
    for k in CLAUDE_CONFIG_DIR CODEX_HOME GROK_HOME CURSOR_CONFIG_DIR \
             XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_CACHE_HOME; do
      printf '%s=%s\n' "$k" "${!k:-unset}"
    done
  } > "${log%/*}/npx.env"
  # The first PATH component, which is where `#!/usr/bin/env node` looks first.
  printf '%s\n' "${PATH%%:*}" > "${log%/*}/npx.path"
fi

code=$(val exit)
[ -z "$code" ] && code=0
if [ "$code" != 0 ]; then
  echo "stub npx: failing on request" >&2
  exit "$code"
fi

verb=${3:-}
if [ $# -ge 3 ]; then
  shift 3
fi

source_dir=$(val source_dir)
source_url=$(val source_url)
skill_path=$(val skill_path)
skills_home="$HOME/.agents/skills"
lock="$HOME/.agents/.skill-lock.json"

lock_set() {
  local name=$1
  python3 - "$lock" "$name" "$source_url" "$skill_path" <<'PY'
import json, os, sys

path, name, url, skill_path = sys.argv[1:5]
data = {}
if os.path.exists(path):
    with open(path) as handle:
        data = json.load(handle)
data.setdefault("version", 3)
data.setdefault("skills", {})
data["skills"][name] = {
    "source": url,
    "sourceType": "github",
    "sourceUrl": url,
    "skillPath": skill_path or f".agents/skills/{name}/SKILL.md",
    "skillFolderHash": f"stub-{name}",
}
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
PY
}

lock_drop() {
  python3 - "$lock" "$@" <<'PY'
import json, os, sys

path = sys.argv[1]
names = sys.argv[2:]
if not os.path.exists(path):
    raise SystemExit(0)
with open(path) as handle:
    data = json.load(handle)
skills = data.get("skills", {})
for name in names:
    skills.pop(name, None)
with open(path, "w") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
PY
}

install_from_source() {
  local name=$1
  [ -n "$name" ] || return 0
  [ -n "$source_dir" ] || return 0
  [ -d "$source_dir/.agents/skills/$name" ] || return 0
  rm -rf "${skills_home:?}/$name"
  mkdir -p "$skills_home"
  cp -R "$source_dir/.agents/skills/$name" "$skills_home/$name"
  lock_set "$name"
}

# The names a verb was given on the command line.
given_names() {
  local name
  while [ $# -gt 0 ]; do
    case $1 in
      -*) ;;
      *) name=$1; printf '%s\n' "$name" ;;
    esac
    shift
  done
}

all_source_names() {
  local dir
  [ -n "$source_dir" ] || return 0
  for dir in "$source_dir"/.agents/skills/*/; do
    [ -d "$dir" ] && basename "$dir"
  done
}

case "$verb" in
  add)
    names=()
    while [ $# -gt 0 ]; do
      case $1 in
        --skill)
          shift
          [ $# -gt 0 ] && names+=("$1")
          ;;
      esac
      shift
    done
    if [ ${#names[@]} -eq 0 ]; then
      while IFS= read -r name; do names+=("$name"); done < <(all_source_names)
    fi
    for name in ${names[@]+"${names[@]}"}; do
      install_from_source "$name"
    done
    ;;

  update)
    names=()
    while IFS= read -r name; do names+=("$name"); done < <(given_names "$@")
    if [ ${#names[@]} -eq 0 ]; then
      while IFS= read -r name; do names+=("$name"); done < <(all_source_names)
    fi
    for name in ${names[@]+"${names[@]}"}; do
      install_from_source "$name"
    done
    ;;

  remove)
    names=()
    while IFS= read -r name; do names+=("$name"); done < <(given_names "$@")
    for name in ${names[@]+"${names[@]}"}; do
      rm -rf "${skills_home:?}/$name"
    done
    if [ ${#names[@]} -gt 0 ]; then
      lock_drop ${names[@]+"${names[@]}"}
    fi
    ;;

  *)
    ;;
esac

exit 0
