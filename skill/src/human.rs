//! Human-readable summaries for the commands people run by hand.
//!
//! `on`, `off`, `render` and `doctor` print one of these instead of the JSON
//! envelope when stdout is a terminal and `--json` was not given. The
//! envelope stays the contract: piped output, `--json` and every other
//! command are unchanged. Each formatter reads the command's own `data`
//! payload and returns `None` for a shape it does not know, which falls back
//! to the envelope.

use std::fmt::Write as _;

use serde_json::Value;

use crate::cli::Command;
use crate::model::{Envelope, Warning};

/// The commands that have a human summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    On,
    Off,
    Render,
    Doctor,
}

/// The summary kind of a parsed command, if it has one.
pub fn kind(command: &Command) -> Option<Kind> {
    match command {
        Command::On(_) => Some(Kind::On),
        Command::Off(_) => Some(Kind::Off),
        Command::Render(_) => Some(Kind::Render),
        Command::Doctor(_) => Some(Kind::Doctor),
        _ => None,
    }
}

/// The summary of a successful run, warnings last; `None` falls back to JSON.
/// A dry run says so first and words every change as one it would make.
pub fn summary(kind: Kind, envelope: &Envelope, dry_run: bool) -> Option<String> {
    let data = &envelope.data;
    let mut out = match kind {
        Kind::On => policy_edit(data, "on", dry_run)?,
        Kind::Off => policy_edit(data, "off", dry_run)?,
        Kind::Render => render(data, dry_run)?,
        Kind::Doctor => doctor(data)?,
    };
    warnings(&mut out, &envelope.meta.warnings);
    Some(out)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn names_of(value: &Value, key: &str) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item[key].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

const DRY_RUN: &str = "dry run: nothing written";

/// `skillherd on` / `skillherd off`: the `sets` edit, then the render it ran.
fn policy_edit(data: &Value, verb: &str, dry_run: bool) -> Option<String> {
    let mut out = String::new();
    if dry_run {
        writeln!(out, "{DRY_RUN}").ok()?;
    }
    let sets = data["sets"].as_str()?;
    if verb == "on" {
        for row in data["rows"].as_array()? {
            let name = row["name"].as_str()?;
            let after = row["after"].as_str().unwrap_or("");
            match row["op"].as_str()? {
                "unchanged" => writeln!(out, "{name}: already `{after}`").ok()?,
                _ if dry_run => writeln!(out, "{name}: would set `{after}` in {sets}").ok()?,
                _ => writeln!(out, "{name}: `{after}` in {sets}").ok()?,
            }
        }
    } else {
        let removed = names_of(&data["removed"], "name");
        if removed.is_empty() {
            writeln!(out, "no row to remove").ok()?;
        } else if dry_run {
            writeln!(out, "would remove {} from {sets}", removed.join(", ")).ok()?;
        } else {
            writeln!(out, "removed {} from {sets}", removed.join(", ")).ok()?;
        }
    }
    match &data["render"] {
        Value::Null => {
            if dry_run && !data["plan"].as_array().is_none_or(Vec::is_empty) {
                writeln!(out, "the surfaces would be rendered to match").ok()?;
            }
        }
        report => out.push_str(&render_body(report, dry_run)?),
    }
    Some(out)
}

/// `skillherd render`: one line per surface that changed or needs attention.
fn render(data: &Value, dry_run: bool) -> Option<String> {
    let body = render_body(data, dry_run)?;
    Some(if dry_run {
        format!("{DRY_RUN}\n{body}")
    } else {
        body
    })
}

/// The per-surface lines of a render report; a dry run's say "would".
fn render_body(data: &Value, dry_run: bool) -> Option<String> {
    let mut out = String::new();
    let mut quiet = 0;
    let [linked, refreshed, removed] = if dry_run {
        ["would link", "would refresh", "would remove"]
    } else {
        ["linked", "refreshed", "removed"]
    };
    for surface in data["surfaces"].as_array()? {
        let name = surface["surface"].as_str()?;
        let mut parts = Vec::new();
        for (key, label) in [
            ("linked", linked),
            ("refreshed", refreshed),
            ("removed", removed),
        ] {
            let names = strings(&surface[key]);
            if !names.is_empty() {
                parts.push(format!("{label} {}", names.join(", ")));
            }
        }
        let refused = names_of(&surface["refused"], "name");
        if !refused.is_empty() {
            parts.push(format!("refused {}", refused.join(", ")));
        }
        let foreign = names_of(&surface["foreign"], "name");
        if !foreign.is_empty() {
            parts.push(format!("left alone (not ours) {}", foreign.join(", ")));
        }
        if parts.is_empty() {
            quiet += 1;
            continue;
        }
        writeln!(out, "{name}: {}", parts.join("; ")).ok()?;
    }
    for agents in data["agents"].as_array().into_iter().flatten() {
        let harness = agents["harness"].as_str()?;
        for (key, label) in [("linked", linked), ("removed", removed)] {
            let names = strings(&agents[key]);
            if !names.is_empty() {
                writeln!(out, "{harness} agents: {label} {}", names.join(", ")).ok()?;
            }
        }
        let blocked = names_of(&agents["blocked"], "name");
        if !blocked.is_empty() {
            writeln!(
                out,
                "{harness} agents: not served for {} (an entry that is not ours holds the path; it is left alone)",
                blocked.join(", ")
            )
            .ok()?;
        }
        let refused = names_of(&agents["refused"], "name");
        if !refused.is_empty() {
            writeln!(out, "{harness} agents: refused {}", refused.join(", ")).ok()?;
        }
        let held = strings(&agents["held"]);
        if !held.is_empty() {
            writeln!(
                out,
                "{harness} agents: not served for {} (list them in .globals/agents to serve them)",
                held.join(", ")
            )
            .ok()?;
        }
    }
    if out.is_empty() {
        writeln!(out, "every surface is up to date").ok()?;
    } else if quiet > 0 {
        writeln!(
            out,
            "{quiet} other surface{} up to date",
            if quiet == 1 { "" } else { "s" }
        )
        .ok()?;
    }
    Some(out)
}

/// `skillherd doctor`: one block per finding, most severe first.
fn doctor(data: &Value) -> Option<String> {
    let findings = data["findings"].as_array()?;
    let mut out = String::new();
    if let Some(reason) = data["refused"].as_str() {
        writeln!(out, "fix refused: {reason}").ok()?;
    }
    let rank = |finding: &Value| match finding["severity"].as_str() {
        Some("error") => 0,
        Some("warn") => 1,
        _ => 2,
    };
    let mut sorted: Vec<&Value> = findings.iter().collect();
    sorted.sort_by_key(|finding| rank(finding));
    for finding in sorted {
        writeln!(
            out,
            "{} {}: {}",
            finding["severity"].as_str().unwrap_or("?"),
            finding["code"].as_str().unwrap_or("?"),
            finding["message"].as_str().unwrap_or("")
        )
        .ok()?;
        if let Some(fix) = finding["suggested_fix"].as_str() {
            writeln!(out, "  fix: {fix}").ok()?;
        }
    }
    for (key, label) in [("fixed", "fixed"), ("failed", "fix failed")] {
        for finding in data[key].as_array().into_iter().flatten() {
            writeln!(
                out,
                "{label}: {} {}",
                finding["code"].as_str().unwrap_or("?"),
                finding["path"].as_str().unwrap_or("")
            )
            .ok()?;
        }
    }
    if out.is_empty() {
        writeln!(out, "healthy: no findings").ok()?;
    }
    Some(out)
}

fn warnings(out: &mut String, warnings: &[Warning]) {
    for warning in warnings {
        let _ = writeln!(out, "warning ({}): {}", warning.code, warning.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_lists_only_surfaces_that_changed() {
        let data = json!({
            "surfaces": [
                {"surface": "claude", "linked": ["a", "b"], "refreshed": [], "removed": ["old"],
                 "unchanged": [], "foreign": [{"name": "mine"}], "refused": []},
                {"surface": "codex", "linked": [], "refreshed": [], "removed": [],
                 "unchanged": ["a"], "foreign": [], "refused": []},
            ],
            "plan": [],
            "agents": [{"harness": "claude", "linked": [], "removed": [], "held": ["imp"]}],
        });
        assert_eq!(
            render(&data, false).unwrap(),
            "claude: linked a, b; removed old; left alone (not ours) mine\n\
             claude agents: not served for imp (list them in .globals/agents to serve them)\n\
             1 other surface up to date\n"
        );
    }

    #[test]
    fn a_quiet_render_says_so() {
        let data = json!({"surfaces": [{"surface": "claude", "linked": [], "refreshed": [],
            "removed": [], "unchanged": ["a"], "foreign": [], "refused": []}], "plan": []});
        assert_eq!(
            render(&data, false).unwrap(),
            "every surface is up to date\n"
        );
    }

    #[test]
    fn doctor_puts_errors_first_and_says_healthy_when_clean() {
        let data = json!({"findings": [
            {"severity": "info", "code": "unknown_provenance", "message": "m1", "path": "/p"},
            {"severity": "error", "code": "dangling_link", "message": "m2", "path": "/q",
             "suggested_fix": "skillherd doctor --fix"},
        ], "fixed": [], "failed": []});
        assert_eq!(
            doctor(&data).unwrap(),
            "error dangling_link: m2\n  fix: skillherd doctor --fix\ninfo unknown_provenance: m1\n"
        );
        let clean = json!({"findings": [], "fixed": [], "failed": []});
        assert_eq!(doctor(&clean).unwrap(), "healthy: no findings\n");
    }

    #[test]
    fn an_unknown_shape_falls_back_to_json() {
        assert!(doctor(&json!({"codes": []})).is_none());
        assert!(render(&json!({}), false).is_none());
        assert!(policy_edit(&json!({}), "on", false).is_none());
    }

    #[test]
    fn on_reports_the_row_and_the_render() {
        let data = json!({
            "sets": "/lib/.globals/sets",
            "rows": [{"name": "foo", "op": "add", "after": "foo main"}],
            "plan": [{"op": "set_row"}],
            "render": {"surfaces": [{"surface": "claude", "linked": ["foo"], "refreshed": [],
                "removed": [], "unchanged": [], "foreign": [], "refused": []}], "plan": []},
        });
        assert_eq!(
            policy_edit(&data, "on", false).unwrap(),
            "foo: `foo main` in /lib/.globals/sets\nclaude: linked foo\n"
        );
    }

    /// Blocked and refused agent paths are reported, not summarised away as
    /// "every surface is up to date".
    #[test]
    fn blocked_and_refused_agents_are_reported() {
        let data = json!({
            "surfaces": [{"surface": "claude", "linked": [], "refreshed": [], "removed": [],
                "unchanged": ["imp"], "foreign": [], "refused": []}],
            "plan": [],
            "agents": [{"harness": "claude", "linked": [], "removed": [], "unchanged": [],
                "blocked": [{"name": "imp-a.md", "path": "/a/imp-a.md"}],
                "refused": [{"name": "imp-b.md", "code": "foreign_entry", "message": "m"}]}],
        });
        assert_eq!(
            render(&data, false).unwrap(),
            "claude agents: not served for imp-a.md (an entry that is not ours holds the path; it is left alone)\n\
             claude agents: refused imp-b.md\n\
             1 other surface up to date\n"
        );
    }

    /// A dry run says so first and words every change as one it would make.
    #[test]
    fn a_dry_run_reads_as_a_plan_not_as_done() {
        let data = json!({
            "surfaces": [{"surface": "claude", "linked": ["a"], "refreshed": ["b"],
                "removed": ["old"], "unchanged": [], "foreign": [], "refused": []}],
            "plan": [{"op": "link"}],
            "agents": [{"harness": "claude", "linked": ["imp-a.md"], "removed": [], "held": []}],
        });
        assert_eq!(
            render(&data, true).unwrap(),
            "dry run: nothing written\n\
             claude: would link a; would refresh b; would remove old\n\
             claude agents: would link imp-a.md\n"
        );
        let on = json!({
            "sets": "/lib/.globals/sets",
            "rows": [{"name": "foo", "op": "add", "after": "foo main"}],
            "plan": [{"op": "set_row"}],
            "render": null,
        });
        assert_eq!(
            policy_edit(&on, "on", true).unwrap(),
            "dry run: nothing written\nfoo: would set `foo main` in /lib/.globals/sets\n\
             the surfaces would be rendered to match\n"
        );
        let off = json!({
            "sets": "/lib/.globals/sets",
            "removed": [{"name": "foo", "lines": []}],
            "plan": [{"op": "set_row"}],
            "render": null,
        });
        assert_eq!(
            policy_edit(&off, "off", true).unwrap(),
            "dry run: nothing written\nwould remove foo from /lib/.globals/sets\n\
             the surfaces would be rendered to match\n"
        );
    }
}
