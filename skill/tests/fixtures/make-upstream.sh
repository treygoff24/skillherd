#!/usr/bin/env bash
# Build a local "upstream" git repository carrying a universal skill and a
# per-harness Claude variant, so reconcile can be exercised without network.
#
#   make-upstream.sh init <work-dir> <name> <variant-body>
#   make-upstream.sh publish <work-dir> <bare-dir>
#
# `init` creates (or refreshes) the work tree: `.agents/skills/<name>/SKILL.md`
# is the universal copy and `.claude/skills/<name>/SKILL.md` carries
# `variant-body`. `publish` commits it and clones it bare into `<bare-dir>`,
# which is what a lock `sourceUrl` points at as `file://<bare-dir>`.
#
# Re-running both is how a test moves upstream: edit the work tree, publish
# again, and the bare repo is replaced in place.
set -euo pipefail

usage() {
  echo "usage: make-upstream.sh init <work-dir> <name> <variant-body>" >&2
  echo "       make-upstream.sh publish <work-dir> <bare-dir>" >&2
  exit 2
}

git_quiet() {
  git -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

case "${1:-}" in
  init)
    [ $# -eq 4 ] || usage
    work=$2
    name=$3
    variant=$4
    mkdir -p "$work/.agents/skills/$name" "$work/.claude/skills/$name"
    if [ ! -d "$work/.git" ]; then
      git_quiet -C "$work" init -q
    fi
    {
      printf -- '---\n'
      printf 'name: %s\n' "$name"
      printf 'description: fixture skill for the reconcile tests\n'
      printf -- '---\n\n'
      printf '# %s\n\nuniversal copy\n' "$name"
    } > "$work/.agents/skills/$name/SKILL.md"
    {
      printf -- '---\n'
      printf 'name: %s\n' "$name"
      printf 'description: fixture claude variant\n'
      printf -- '---\n\n'
      printf '# %s\n\n%s\n' "$name" "$variant"
    } > "$work/.claude/skills/$name/SKILL.md"
    ;;

  publish)
    [ $# -eq 3 ] || usage
    work=$2
    bare=$3
    git_quiet -C "$work" add -A
    if ! git_quiet -C "$work" diff --cached --quiet; then
      git_quiet -C "$work" commit -q -m "upstream: $RANDOM"
    fi
    rm -rf "$bare"
    git_quiet clone -q --bare "$work" "$bare"
    ;;

  *)
    usage
    ;;
esac
