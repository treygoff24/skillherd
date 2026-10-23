//! The wire types: envelopes, plans, policy, reports and import records.
//!
//! Every success payload is a typed struct with `schemars::JsonSchema`, so
//! `skillherd schema output` describes the real contract; `data` in the envelope is
//! the serialized form of one of these.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The schema version every envelope carries.
pub const SCHEMA_VERSION: &str = "1.0";

/// Success envelope written to stdout.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Envelope {
    /// Always true; failures are written to stderr as `ErrorEnvelope`.
    pub ok: bool,
    /// The command's typed payload.
    pub data: Value,
    pub meta: Meta,
}

/// Error envelope written to stderr.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorEnvelope {
    /// Always false.
    pub ok: bool,
    pub error: ErrorBody,
    pub meta: Meta,
}

/// The machine-readable half of an error: an agent branches on `code`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<String>,
}

/// Envelope metadata: schema version, correlation id, resolved paths, warnings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Meta {
    pub schema_version: String,
    pub correlation_id: String,
    /// The resolved roots every path in the payload is relative to; null when
    /// the paths themselves could not be resolved.
    pub paths: Option<PathsInfo>,
    pub warnings: Vec<Warning>,
}

/// A non-fatal finding a command reports alongside its payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// The serializable mirror of `Paths::resolve()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PathsInfo {
    pub library: PathBuf,
    pub agents_dir: PathBuf,
    pub lock: PathBuf,
    pub stage: PathBuf,
    pub claude_shared: PathBuf,
    pub codex_home: PathBuf,
    pub grok_home: PathBuf,
    pub pi_skills: PathBuf,
    pub cursor_skills: PathBuf,
    pub platform: Platform,
    pub npx: PathBuf,
    pub npx_pkg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh: Option<PathBuf>,
    pub home: PathBuf,
}

/// One planned filesystem operation, used by every `--dry-run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlanOp {
    /// Short verb: `link`, `unlink`, `remove`, `refresh`, `write`, `delete`,
    /// `restore` (a displaced copy back to its live path), `git`, `npx`.
    pub op: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
}

/// Doctor finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// The tier a harness row serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Main,
    Lane,
    None,
}

/// The machine axis: `mac` or `linux` (`cell` is a legacy input alias).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Mac,
    #[serde(alias = "cell")]
    Linux,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Mac => "mac",
            Self::Linux => "linux",
        })
    }
}

/// One root a harness loads skills from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadRoot {
    Surface,
    AgentsDir,
}

/// The parsed policy: `sets` rows plus the harness table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Policy {
    pub rows: Vec<SetRow>,
    pub harnesses: Vec<HarnessRow>,
    /// The `sets` file verbatim, so a save can preserve comments and ordering.
    pub raw_lines: Vec<String>,
}

impl Policy {
    pub fn harness(&self, name: &str) -> Option<&HarnessRow> {
        self.harnesses.iter().find(|row| row.name == name)
    }

    pub fn row(&self, name: &str) -> Option<&SetRow> {
        self.rows.iter().find(|row| row.name == name)
    }
}

/// One always-on skill row of `library/.globals/sets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetRow {
    pub name: String,
    pub main: bool,
    pub lane: bool,
    pub not_platform: Vec<Platform>,
    pub only: Vec<String>,
    pub not: Vec<String>,
    /// 1-based line in the `sets` file.
    pub line: usize,
}

/// One row of `library/.globals/harnesses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessRow {
    pub name: String,
    pub tier: Tier,
    pub surface: PathBuf,
    pub variant_key: Option<String>,
    pub reads: Vec<ReadRoot>,
}

/// What the Python stripper reported for one surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StripperReport {
    pub rendered: Vec<String>,
    pub refreshed: Vec<String>,
    pub unchanged: Vec<String>,
    pub errors: Vec<StripperError>,
}

/// One stripper failure; a non-empty `errors` fails the render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StripperError {
    pub name: String,
    pub message: String,
}

/// One skill imported from the inbox into the pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Imported {
    pub name: String,
    pub hash: String,
    /// Harness variant keys imported beside the universal copy.
    pub variants: Vec<String>,
}

/// One name a mutator refused to touch, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Refusal {
    pub name: String,
    pub code: String,
    pub message: String,
}

/// `library/.npx/<name>.json`: the import record for one upstream skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NpxMeta {
    pub source: String,
    pub source_type: String,
    pub source_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    pub skill_path: String,
    pub skill_folder_hash: String,
    /// Tree hash of the entry at import; a different hash means local edits.
    pub imported_hash: String,
    pub imported_at: String,
    pub revision: String,
    /// Per-variant tree hash at import, keyed by harness.
    pub variants: BTreeMap<String, String>,
    pub variants_pending: bool,
    /// Per-harness tree hash of the upstream agents stored under
    /// `.harness-agents/<harness>` at import. Omitted when empty, so a record
    /// for a skill that ships no agents is byte-identical to one written
    /// before agents were imported.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub harness_agents: BTreeMap<String, String>,
    /// Where a `github-release` record's upstream zip lives. Omitted for npx
    /// imports, so their records stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseSource>,
}

/// The `sourceType` of a skill that ships as a zip asset on GitHub releases.
pub const SOURCE_GITHUB_RELEASE: &str = "github-release";

/// A skill shipped as a zip asset on a repository's GitHub releases: the
/// newest stable release whose tag starts with `tagPrefix` carries `asset`,
/// and `dir` inside the zip is the skill. The record's `revision` is the tag
/// last imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSource {
    /// `owner/repo`.
    pub repo: String,
    /// The zip asset's file name.
    pub asset: String,
    pub tag_prefix: String,
    /// The directory inside the zip that holds `SKILL.md`.
    pub dir: String,
}

/// The current time as RFC 3339 UTC, for import records. Computed from
/// `SystemTime` so the crate stays dependency-free.
pub fn now_iso8601() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let (year, month, day, hour, minute, second) = civil_from_unix(seconds);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days-to-civil conversion (Howard Hinnant's algorithm), UTC.
fn civil_from_unix(unix: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let seconds_of_day = unix.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        month,
        day,
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_matches_known_instants() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_from_unix(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        assert_eq!(civil_from_unix(1_700_000_000), (2023, 11, 14, 22, 13, 20));
        assert_eq!(civil_from_unix(951_782_400), (2000, 2, 29, 0, 0, 0));
    }

    #[test]
    fn now_is_rfc3339_utc() {
        let now = now_iso8601();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(&now[4..5], "-");
        assert_eq!(&now[10..11], "T");
        assert!(now.starts_with("20"), "{now}");
    }

    #[test]
    fn npx_meta_uses_the_documented_keys() {
        let record = NpxMeta {
            source: "owner/repo".into(),
            source_type: "github".into(),
            source_url: "https://github.com/owner/repo".into(),
            r#ref: Some("main".into()),
            skill_path: ".agents/skills/foo/SKILL.md".into(),
            skill_folder_hash: "abc".into(),
            imported_hash: "def".into(),
            imported_at: now_iso8601(),
            revision: "0".repeat(40),
            variants: BTreeMap::from([("claude".to_string(), "ghi".to_string())]),
            variants_pending: false,
            harness_agents: BTreeMap::from([("claude".to_string(), "jkl".to_string())]),
            release: None,
        };
        let value = serde_json::to_value(&record).unwrap();
        for key in [
            "source",
            "sourceType",
            "sourceUrl",
            "ref",
            "skillPath",
            "skillFolderHash",
            "importedHash",
            "importedAt",
            "revision",
            "variants",
            "variantsPending",
            "harnessAgents",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["variants"]["claude"], "ghi");
        assert!(
            value.get("release").is_none(),
            "an npx record carries no release key"
        );
        let back: NpxMeta = serde_json::from_value(value).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn tiers_and_platforms_serialize_lowercase() {
        assert_eq!(serde_json::to_value(Tier::Lane).unwrap(), "lane");
        assert_eq!(serde_json::to_value(Tier::None).unwrap(), "none");
        assert_eq!(serde_json::to_value(Platform::Linux).unwrap(), "linux");
        assert_eq!(
            serde_json::to_value(ReadRoot::AgentsDir).unwrap(),
            "agents_dir"
        );
        assert_eq!(serde_json::to_value(Severity::Warn).unwrap(), "warn");
    }
}
