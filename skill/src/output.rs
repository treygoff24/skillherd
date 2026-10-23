//! The envelope writers: success JSON on stdout, error JSON on stderr.

use std::cell::{Cell, RefCell};
use std::io::{self, BufWriter, Read, Write};
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::Value;

use crate::error::{EXIT_SUCCESS, Error};
use crate::hash;
use crate::model::{Envelope, ErrorBody, ErrorEnvelope, Meta, SCHEMA_VERSION, Warning};
use crate::paths::Paths;

// The correlation id is runtime input (`--correlation-id`), so it needs a
// set-once cell rather than a `LazyLock` initializer.
static CORRELATION_ID: OnceLock<String> = OnceLock::new();

thread_local! {
    static WARNINGS: RefCell<Vec<Warning>> = const { RefCell::new(Vec::new()) };
    static EXIT_CODE: Cell<i32> = const { Cell::new(EXIT_SUCCESS) };
}

/// Adopt `--correlation-id` when the caller gave one; later calls are ignored
/// so the whole run reports one id.
pub fn set_correlation_id(id: Option<String>) {
    if let Some(id) = id {
        let _ = CORRELATION_ID.set(id);
    }
}

/// The process correlation id, generated on first use.
pub fn correlation_id() -> String {
    CORRELATION_ID.get_or_init(random_uuid_v4).clone()
}

/// A uuid-v4-shaped id from `/dev/urandom`, falling back to a hash of the
/// clock and the pid so a restricted environment still gets a usable id.
pub fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    if !read_urandom(&mut bytes) {
        let mut hasher = hash::Sha256::new();
        hasher.update_str(&std::process::id().to_string());
        hasher.update_str(
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| format!("{}:{}", elapsed.as_secs(), elapsed.subsec_nanos()))
                .unwrap_or_else(|_| "unknown".to_string()),
        );
        hasher.update_str(&format!("{:p}", &bytes));
        bytes.copy_from_slice(&hasher.finish());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10xx
    let hex = hash::hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn read_urandom(bytes: &mut [u8; 16]) -> bool {
    match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(bytes)) {
        Ok(()) => true,
        Err(_) => false,
    }
}

/// Record a non-fatal warning; it is reported in `meta.warnings`.
pub fn warn(code: &str, message: impl Into<String>) {
    WARNINGS.with(|warnings| {
        warnings.borrow_mut().push(Warning {
            code: code.to_string(),
            message: message.into(),
        });
    });
}

/// Record a warning-class error (`variants_unavailable`) as a warning.
pub fn warn_error(err: &Error) {
    warn(err.code(), err.to_string());
}

/// The warnings collected so far.
pub fn warnings() -> Vec<Warning> {
    WARNINGS.with(|warnings| warnings.borrow().clone())
}

/// Drop collected warnings (tests only).
pub fn clear_warnings() {
    WARNINGS.with(|warnings| warnings.borrow_mut().clear());
}

/// Ask the process to exit with `code` after its success envelope is written.
///
/// `doctor` is the linter-style command that needs this: findings are not a
/// failure, so its report goes to stdout as usual while the exit status
/// carries 1 / 2 / 3 / 4 from the published dictionary.
pub fn request_exit_code(code: i32) {
    EXIT_CODE.with(|cell| cell.set(code));
}

/// The exit code the run asked for; 0 unless a command requested otherwise.
pub fn exit_code() -> i32 {
    EXIT_CODE.with(Cell::get)
}

/// Envelope metadata for this run.
pub fn meta(paths: Option<&Paths>) -> Meta {
    Meta {
        schema_version: SCHEMA_VERSION.to_string(),
        correlation_id: correlation_id(),
        paths: paths.map(|paths| paths.info()),
        warnings: warnings(),
    }
}

/// Build the success envelope for a command payload.
pub fn success_envelope(data: Value, paths: Option<&Paths>) -> Envelope {
    Envelope {
        ok: true,
        data,
        meta: meta(paths),
    }
}

/// Build the error envelope for a failure.
pub fn error_envelope(err: &Error, paths: Option<&Paths>) -> ErrorEnvelope {
    ErrorEnvelope {
        ok: false,
        error: ErrorBody {
            code: err.code().to_string(),
            message: err.to_string(),
            details: err.details().clone(),
            retryable: err.retryable(),
            suggested_fix: err.suggested_fix().map(str::to_owned),
        },
        meta: meta(paths),
    }
}

/// Write the success envelope to stdout as one JSON document.
pub fn write_success(envelope: &Envelope, pretty: bool) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    write_json(&mut out, envelope, pretty)?;
    out.flush()
}

/// Write the error envelope to stderr as one JSON document.
pub fn write_error(err: &Error, paths: Option<&Paths>, pretty: bool) -> io::Result<()> {
    let stderr = io::stderr();
    let mut out = BufWriter::new(stderr.lock());
    write_json(&mut out, &error_envelope(err, paths), pretty)?;
    out.flush()
}

fn write_json<T: Serialize>(out: &mut impl Write, value: &T, pretty: bool) -> io::Result<()> {
    if pretty {
        serde_json::to_writer_pretty(&mut *out, value)?;
    } else {
        serde_json::to_writer(&mut *out, value)?;
    }
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn correlation_ids_are_uuid_v4_shaped_and_unique() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = random_uuid_v4();
            assert_eq!(id.len(), 36, "{id}");
            let parts: Vec<&str> = id.split('-').collect();
            assert_eq!(
                parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12],
                "{id}"
            );
            assert!(
                id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
                "{id}"
            );
            assert_eq!(&id[14..15], "4", "version nibble: {id}");
            assert!(
                matches!(&id[19..20], "8" | "9" | "a" | "b"),
                "variant nibble: {id}"
            );
            assert!(
                id.chars().all(|c| !c.is_ascii_uppercase()),
                "lowercase: {id}"
            );
            assert!(seen.insert(id.clone()), "duplicate id {id}");
        }
    }

    #[test]
    fn the_process_keeps_one_correlation_id() {
        let first = correlation_id();
        let second = correlation_id();
        assert_eq!(first, second);
        assert_eq!(correlation_id().len(), 36);
    }

    #[test]
    fn warnings_are_collected_and_reported_in_meta() {
        clear_warnings();
        warn("variants_unavailable", "offline");
        warn_error(&Error::variants_unavailable("foo", "offline"));
        let meta = meta(None);
        assert_eq!(meta.warnings.len(), 2);
        assert_eq!(meta.warnings[0].code, "variants_unavailable");
        assert!(meta.warnings[1].message.contains("foo"));
        assert_eq!(meta.schema_version, "1.0");
        assert!(meta.paths.is_none());
        clear_warnings();
        assert!(warnings().is_empty());
    }

    #[test]
    fn a_command_can_request_a_linter_style_exit_code() {
        use crate::error::{EXIT_DOCTOR_FINDINGS, EXIT_SUCCESS};
        assert_eq!(exit_code(), EXIT_SUCCESS);
        request_exit_code(EXIT_DOCTOR_FINDINGS);
        assert_eq!(exit_code(), 1, "doctor findings are not a failure");
        request_exit_code(EXIT_SUCCESS);
        assert_eq!(exit_code(), EXIT_SUCCESS);
    }

    #[test]
    fn a_success_envelope_carries_the_resolved_paths() {
        let paths = Paths::fixture("/sandbox/user");
        let envelope = success_envelope(serde_json::json!({ "n": 1 }), Some(&paths));
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["n"], 1);
        assert_eq!(value["meta"]["schema_version"], "1.0");
        assert_eq!(
            value["meta"]["paths"]["library"],
            "/sandbox/user/.agents/skill-library"
        );
        assert_eq!(
            value["meta"]["paths"]["platform"],
            serde_json::to_value(paths.platform).unwrap()
        );
        assert_eq!(value["meta"]["warnings"], serde_json::json!([]));
    }

    #[test]
    fn an_error_envelope_carries_the_machine_contract() {
        let paths = Paths::fixture("/sandbox/user");
        let err = Error::invalid_name("bad/name", "contains '/'");
        let value = serde_json::to_value(error_envelope(&err, Some(&paths))).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_name");
        assert_eq!(value["error"]["retryable"], false);
        assert!(value["error"]["suggested_fix"].is_string());
        assert_eq!(value["error"]["details"]["name"], "bad/name");
        assert!(value["meta"]["correlation_id"].is_string());
        assert_eq!(
            value["meta"]["paths"]["library"],
            "/sandbox/user/.agents/skill-library"
        );
    }

    #[test]
    fn the_envelope_survives_a_round_trip_in_both_styles() {
        let envelope = success_envelope(serde_json::json!({ "ok": true }), None);
        let compact = serde_json::to_string(&envelope).unwrap();
        let pretty = serde_json::to_string_pretty(&envelope).unwrap();
        assert!(pretty.contains('\n'), "--pretty indents");
        assert!(!compact.contains('\n'));
        let parsed: Envelope = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed.data["ok"], true);
        assert!(
            parsed.meta.paths.is_none(),
            "paths serialize as null when unknown"
        );
    }
}
