"""Regenerate expected.json from the Python stripper this port replaced.

    git show e7b1a16^:bin/skill-render > "$TMPDIR/skill_render.py"
    python3 -I skill/tests/fixtures/strip/bytes/generate.py "$TMPDIR/skill_render.py"

Each `*.md` file here is an input, read as raw bytes (BOMs, invalid UTF-8,
NUL, form feeds). For every platform and harness key the expected output is
the hex of what `strip_bytes` returned and whether it saw markers, or the
error line and reason it raised.
"""
import importlib.machinery
import importlib.util
import json
import os
import sys

here = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("skill_render", sys.argv[1])
spec = importlib.util.spec_from_loader("skill_render", loader)
render = importlib.util.module_from_spec(spec)
loader.exec_module(render)

cases = {}
for name in sorted(os.listdir(here)):
    if not name.endswith(".md"):
        continue
    with open(os.path.join(here, name), "rb") as handle:
        data = handle.read()
    outputs = {}
    for keep in ("mac", "cell"):
        for harness in ("-", "claude", "codex", "lane"):
            key = "%s/%s" % (keep, harness)
            try:
                out, had = render.strip_bytes(
                    data, keep, harness=None if harness == "-" else harness
                )
                outputs[key] = {"ok_hex": out.hex(), "had_markers": had}
            except render.MarkerError as err:
                outputs[key] = {"error": {"line": err.lineno, "reason": err.reason}}
    cases[name] = outputs

json.dump(
    {
        "generated_by": "the Python bin/skill-render strip_bytes (RENDER_VERSION %d) via generate.py"
        % render.RENDER_VERSION,
        "cases": cases,
    },
    sys.stdout if len(sys.argv) < 3 else open(sys.argv[2], "w"),
    indent=1,
    sort_keys=True,
)
print()
