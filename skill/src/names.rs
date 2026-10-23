//! Skill names are one safe path component.

use crate::error::{Error, Result};

/// Longest accepted name.
pub const MAX_NAME_LEN: usize = 64;

/// Validate a skill name before it is ever joined to a path.
///
/// Grammar: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`. `.` and `..` are excluded by
/// the first-character rule, so no name can be a dot-entry or a traversal.
pub fn validate_name(s: &str) -> Result<&str> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err(Error::invalid_name(s, "it is empty"));
    }
    if bytes.len() > MAX_NAME_LEN {
        return Err(Error::invalid_name(s, "it is longer than 64 bytes"));
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return Err(Error::invalid_name(
            s,
            "the first character must be an ASCII letter or digit",
        ));
    }
    for &byte in &bytes[1..] {
        let ok = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        if !ok {
            return Err(Error::invalid_name(
                s,
                "only ASCII letters, digits, '.', '_' and '-' are allowed",
            ));
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_skill_names() {
        for name in [
            "post",
            "writing-plans",
            "talking-to-user",
            "impeccable",
            "a",
            "skill.v2",
            "no_ceremony",
            "AgentBrowser",
            "0",
        ] {
            assert_eq!(validate_name(name).unwrap(), name, "{name} should be valid");
        }
    }

    #[test]
    fn accepts_exactly_sixty_four_bytes() {
        let name = format!("a{}", "b".repeat(63));
        assert_eq!(name.len(), 64);
        assert!(validate_name(&name).is_ok());
        let too_long = format!("a{}", "b".repeat(64));
        assert_eq!(validate_name(&too_long).unwrap_err().code(), "invalid_name");
    }

    #[test]
    fn rejects_traversal_dot_entries_and_separators() {
        for name in [
            "",
            ".",
            "..",
            "../etc",
            "a/b",
            "/abs",
            ".hidden",
            "-leading",
            "_leading",
            ".skill-render.json",
            "a b",
            "a\tb",
            "naïve",
            "café",
            "a\u{0}b",
        ] {
            let err = validate_name(name).unwrap_err();
            assert_eq!(err.code(), "invalid_name", "{name:?} should be rejected");
            assert_eq!(err.exit_code(), 65);
        }
    }

    #[test]
    fn the_error_names_the_offending_input() {
        let err = validate_name("../etc").unwrap_err();
        assert!(err.to_string().contains("../etc"), "{err}");
        assert_eq!(err.details()["name"], "../etc");
    }
}
