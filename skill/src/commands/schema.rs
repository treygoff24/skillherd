//! `skillherd schema` — the machine contract, so an agent never needs the docs.

use clap::CommandFactory;
use schemars::JsonSchema;
use serde_json::{Map, Value, json};

use crate::cli::{Cli, SchemaArgs, SchemaTarget};
use crate::commands::sync::SyncReport;
use crate::doctor::{DoctorCapabilities, DoctorExitCode, DoctorReport, Finding, FindingCapability};
use crate::error::{Error, Result};
use crate::library::SkillEntry;
use crate::managed::{Entry, EntryKind};
use crate::model::{
    Envelope, ErrorBody, ErrorEnvelope, HarnessRow, Imported, Meta, NpxMeta, PathsInfo, PlanOp,
    Policy, ReadRoot, Refusal, SetRow, Severity, StripperError, StripperReport, Tier, Warning,
};
use crate::paths::Paths;
use crate::project::{
    ProjectActivation, ProjectAddReport, ProjectDrop, ProjectDropReport, ProjectSurface,
};
use crate::reconcile::{ReconcileOpts, ReconcileReport};
use crate::render::{RenderOpts, RenderReport, SurfaceReport};

/// Print the requested section of the contract.
pub fn run(args: &SchemaArgs, _paths: &Paths, _dry_run: bool) -> Result<Value> {
    Ok(match args.target {
        SchemaTarget::All => json!({
            "input": input_contract(),
            "output": output_contract(),
            "error": error_contract(),
        }),
        SchemaTarget::Input => input_contract(),
        SchemaTarget::Output => output_contract(),
        SchemaTarget::Error => error_contract(),
    })
}

/// The blast radius of each command, for an agent choosing what to call.
const BLAST_RADIUS: [(&str, &str); 17] = [
    (
        "npx",
        "external command; managed skills mutations support --dry-run",
    ),
    ("schema", "read-only"),
    ("doctor", "read-only unless --fix"),
    ("list", "read-only"),
    ("search", "read-only"),
    ("info", "read-only"),
    ("on", "mutating, --dry-run supported"),
    ("off", "mutating, --dry-run supported"),
    ("add", "mutating, --dry-run supported"),
    ("drop", "mutating, --dry-run supported"),
    ("render", "mutating, --dry-run supported"),
    ("reconcile", "mutating, --dry-run supported"),
    ("install", "mutating, --dry-run supported"),
    ("update", "mutating, --dry-run supported"),
    ("remove", "mutating, --dry-run supported"),
    ("sync", "mutating, no --dry-run"),
    ("strip", "read-only unless --in-place, --dry-run supported"),
];

fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({ "type": "unknown" }))
}

macro_rules! payload_schemas {
    ($($name:literal => $type:ty),* $(,)?) => {{
        let mut payloads = Map::new();
        $(payloads.insert($name.to_string(), schema_of::<$type>());)*
        Value::Object(payloads)
    }};
}

/// Schemas for every payload type `data` can carry.
fn output_contract() -> Value {
    json!({
        "envelope": schema_of::<Envelope>(),
        "payloads": payload_schemas! {
            "Envelope" => Envelope,
            "Meta" => Meta,
            "PathsInfo" => PathsInfo,
            "Warning" => Warning,
            "PlanOp" => PlanOp,
            "Policy" => Policy,
            "SetRow" => SetRow,
            "HarnessRow" => HarnessRow,
            "Tier" => Tier,
            "Severity" => Severity,
            "ReadRoot" => ReadRoot,
            "StripperReport" => StripperReport,
            "StripperError" => StripperError,
            "Imported" => Imported,
            "Refusal" => Refusal,
            "NpxMeta" => NpxMeta,
            "Entry" => Entry,
            "EntryKind" => EntryKind,
            "SkillEntry" => SkillEntry,
            "RenderOpts" => RenderOpts,
            "RenderReport" => RenderReport,
            "SurfaceReport" => SurfaceReport,
            "ReconcileOpts" => ReconcileOpts,
            "ReconcileReport" => ReconcileReport,
            "DoctorReport" => DoctorReport,
            "Finding" => Finding,
            "DoctorCapabilities" => DoctorCapabilities,
            "FindingCapability" => FindingCapability,
            "DoctorExitCode" => DoctorExitCode,
            "ProjectAddReport" => ProjectAddReport,
            "ProjectActivation" => ProjectActivation,
            "ProjectSurface" => ProjectSurface,
            "ProjectDropReport" => ProjectDropReport,
            "ProjectDrop" => ProjectDrop,
            "SyncReport" => SyncReport,
            "PoolSyncReport" => crate::commands::sync::PoolSyncReport,
            "StripReport" => crate::commands::strip::StripReport,
            "StrippedFile" => crate::commands::strip::StrippedFile,
        },
        "note": "`data` is the serialized form of one of these payloads; `skillherd schema input` lists each command's flags.",
    })
}

/// The error envelope, the code dictionary and the exit codes.
fn error_contract() -> Value {
    json!({
        "envelope": schema_of::<ErrorEnvelope>(),
        "error_body": schema_of::<ErrorBody>(),
        "codes": Error::dictionary(),
        "exit_codes": {
            "0": "success (and doctor: healthy)",
            "1": "doctor: findings; the report is on stdout",
            "2": "usage error, or doctor: partial fix; a doctor report is on stdout",
            "3": "doctor: fix failed; the report is on stdout",
            "4": "doctor: refused as unsafe; the report is on stdout",
            "65": "invalid data",
            "66": "missing input",
            "70": "internal error",
            "75": "temporary external failure (retryable)",
            "77": "permission denied",
            "78": "configuration error",
        },
    })
}

/// The Clap-derived input surface.
fn input_contract() -> Value {
    let mut command = Cli::command();
    command.build();
    let mut described = describe_command(&command);
    if let Some(object) = described.as_object_mut() {
        object.insert(
            "blast_radius".to_string(),
            Value::Object(
                BLAST_RADIUS
                    .iter()
                    .map(|(name, radius)| ((*name).to_string(), json!(radius)))
                    .collect(),
            ),
        );
    }
    described
}

fn describe_command(command: &clap::Command) -> Value {
    let arguments: Vec<Value> = command
        .get_arguments()
        .filter(|argument| !argument.is_hide_set())
        .map(describe_argument)
        .collect();
    let subcommands: Vec<Value> = command.get_subcommands().map(describe_command).collect();
    json!({
        "name": command.get_name(),
        "version": command.get_version(),
        "about": command.get_about().map(|about| about.to_string()),
        "args": arguments,
        "subcommands": subcommands,
    })
}

fn describe_argument(argument: &clap::Arg) -> Value {
    let possible_values: Vec<String> = argument
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_string())
        .collect();
    json!({
        "id": argument.get_id().to_string(),
        "long": argument.get_long(),
        "short": argument.get_short().map(|short| short.to_string()),
        "value_names": argument.get_value_names().map(|names| {
            names.iter().map(|name| name.to_string()).collect::<Vec<_>>()
        }),
        "required": argument.is_required_set(),
        "takes_value": argument.get_action().takes_values(),
        "global": argument.is_global_set(),
        "possible_values": possible_values,
        "help": argument.get_help().map(|help| help.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn run_target(target: SchemaTarget) -> Value {
        let dir = TempDir::new().unwrap();
        let paths = Paths::fixture(dir.path());
        run(&SchemaArgs { target }, &paths, false).unwrap()
    }

    #[test]
    fn every_payload_type_is_named_once() {
        let output = run_target(SchemaTarget::Output);
        let payloads = output["payloads"].as_object().unwrap();
        for name in [
            "Envelope",
            "Meta",
            "PathsInfo",
            "Warning",
            "PlanOp",
            "Policy",
            "SetRow",
            "HarnessRow",
            "Tier",
            "Severity",
            "ReadRoot",
            "StripperReport",
            "StripperError",
            "Imported",
            "Refusal",
            "NpxMeta",
            "Entry",
            "EntryKind",
        ] {
            assert!(payloads.contains_key(name), "missing payload {name}");
            assert!(
                payloads[name].is_object(),
                "{name} should be a JSON Schema object"
            );
        }
        assert!(output["envelope"].is_object());
    }

    #[test]
    fn the_error_contract_publishes_every_code_and_exit() {
        let error = run_target(SchemaTarget::Error);
        let codes: Vec<&str> = error["codes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["code"].as_str().unwrap())
            .collect();
        for code in [
            "usage",
            "invalid_name",
            "input_not_found",
            "config_error",
            "library_busy",
            "library_dirty_unrelated",
            "local_edit_refused",
            "npx_failed",
            "git_failed",
            "render_failed",
            "policy_bad_token",
            "policy_lane_not_in_main",
            "policy_unknown_harness",
            "policy_unrepresentable",
            "variants_unavailable",
            "internal",
            "not_implemented",
        ] {
            assert!(codes.contains(&code), "missing error code {code}");
        }
        assert_eq!(error["exit_codes"]["78"], "configuration error");
        assert!(error["envelope"].is_object());
        assert!(error["error_body"].is_object());
    }

    #[test]
    fn the_input_contract_covers_every_command_and_flag() {
        let input = run_target(SchemaTarget::Input);
        let names: Vec<&str> = input["subcommands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|sub| sub["name"].as_str().unwrap())
            .collect();
        for name in [
            "schema",
            "doctor",
            "list",
            "search",
            "info",
            "on",
            "off",
            "add",
            "drop",
            "render",
            "reconcile",
            "install",
            "update",
            "remove",
            "sync",
            "strip",
        ] {
            assert!(names.contains(&name), "missing subcommand {name}");
        }
        assert_eq!(names.len(), BLAST_RADIUS.len());

        let on = input["subcommands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|sub| sub["name"] == "on")
            .unwrap();
        let longs: Vec<&str> = on["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|arg| arg["long"].as_str())
            .collect();
        for flag in [
            "tier", "only", "not", "no-mac", "no-linux", "dry-run", "pretty",
        ] {
            assert!(longs.contains(&flag), "on is missing --{flag}");
        }
        assert_eq!(input["blast_radius"]["sync"], "mutating, no --dry-run");
    }

    #[test]
    fn all_is_the_three_contracts_together() {
        let all = run_target(SchemaTarget::All);
        assert_eq!(all["input"], run_target(SchemaTarget::Input));
        assert_eq!(all["output"], run_target(SchemaTarget::Output));
        assert_eq!(all["error"], run_target(SchemaTarget::Error));
    }
}
