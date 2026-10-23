//! Parsing of SMTP paths — the `<...>` in `MAIL FROM:` and `RCPT TO:`.
//!
//! This is **transport syntax**, not address validation. The two are separate
//! concerns and conflating them would break the product: Sandesh exists partly
//! so developers can test unusual addresses, so this layer establishes only
//! where the path ends and the parameters begin.
//!
//! Semantic interpretation is
//! [`EmailAddress::parse_lenient`](sandesh_core::EmailAddress::parse_lenient),
//! which never fails.

use std::fmt;

/// A path and whatever followed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPath {
    /// The address between the angle brackets. Empty for `<>`.
    pub path: String,
    /// Text after the closing bracket — ESMTP parameters.
    pub remainder: String,
}

/// Why a path could not be delimited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// No `<` was found where one was required.
    MissingOpenBracket,
    /// A `<` was found with no matching `>`.
    UnclosedBracket,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOpenBracket => f.write_str("expected an address in angle brackets"),
            Self::UnclosedBracket => f.write_str("unterminated angle bracket"),
        }
    }
}

/// Extracts the bracketed path from the text following `FROM:` or `TO:`.
///
/// RFC 5321 §4.1.2 requires the brackets. A bare address without them is also
/// accepted, because clients send it and refusing would make Sandesh worse at
/// its job for no protocol benefit.
///
/// A source route (`@relay:user@host`, RFC 5321 §4.1.2) is stripped and the
/// final address kept — the RFC requires servers to ignore it.
///
/// # Errors
/// Returns [`PathError`] only when the delimiters are unusable.
pub fn parse_path(input: &str) -> Result<ParsedPath, PathError> {
    let input = input.trim_start();

    let Some(rest) = input.strip_prefix('<') else {
        // No brackets: take the first whitespace-separated token as the path.
        if input.is_empty() {
            return Err(PathError::MissingOpenBracket);
        }
        let (path, remainder) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
        return Ok(ParsedPath {
            path: strip_source_route(path).to_owned(),
            remainder: remainder.trim().to_owned(),
        });
    };

    // Find the closing bracket. Scanning from the end rather than the start
    // means a quoted local part containing '>' does not truncate the address.
    let close = rest.rfind('>').ok_or(PathError::UnclosedBracket)?;

    Ok(ParsedPath {
        path: strip_source_route(&rest[..close]).to_owned(),
        remainder: rest[close + 1..].trim().to_owned(),
    })
}

/// Removes an RFC 5321 §4.1.2 source route, keeping the final address.
///
/// `@relay1,@relay2:user@host` becomes `user@host`. Servers are required to
/// ignore the route and deliver to the final destination.
fn strip_source_route(path: &str) -> &str {
    if !path.starts_with('@') {
        return path;
    }
    path.find(':').map_or(path, |colon| &path[colon + 1..])
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_bracketed_address() {
        let parsed = parse_path("<alice@example.test>").unwrap_or(ParsedPath {
            path: String::new(),
            remainder: String::new(),
        });
        assert_eq!(parsed.path, "alice@example.test");
        assert!(parsed.remainder.is_empty());
    }

    #[test]
    fn recognises_the_null_path() {
        let parsed = parse_path("<>").unwrap_or(ParsedPath {
            path: String::from("x"),
            remainder: String::new(),
        });
        assert_eq!(parsed.path, "", "the null reverse-path is an empty path");
    }

    #[test]
    fn separates_esmtp_parameters() {
        let parsed = parse_path("<a@b.test> SIZE=100 BODY=8BITMIME").unwrap_or(ParsedPath {
            path: String::new(),
            remainder: String::new(),
        });
        assert_eq!(parsed.path, "a@b.test");
        assert_eq!(parsed.remainder, "SIZE=100 BODY=8BITMIME");
    }

    #[test]
    fn accepts_an_unbracketed_address() {
        // Not RFC-conformant, but clients do it and rejecting helps nobody.
        let parsed = parse_path("alice@example.test SIZE=10").unwrap_or(ParsedPath {
            path: String::new(),
            remainder: String::new(),
        });
        assert_eq!(parsed.path, "alice@example.test");
        assert_eq!(parsed.remainder, "SIZE=10");
    }

    #[test]
    fn strips_a_source_route() {
        // RFC 5321 §4.1.2 requires the route to be ignored.
        let parsed =
            parse_path("<@relay1.test,@relay2.test:user@final.test>").unwrap_or(ParsedPath {
                path: String::new(),
                remainder: String::new(),
            });
        assert_eq!(parsed.path, "user@final.test");
    }

    #[test]
    fn preserves_unusual_but_legal_local_parts() {
        // Sandesh exists partly to test these; the parser must not mangle them.
        for address in [
            r#""odd@name"@example.test"#,
            "user+tag@example.test",
            "user.name@example.test",
            "!#$%&'*+-/=?^_`{|}~@example.test",
        ] {
            let parsed = parse_path(&format!("<{address}>")).unwrap_or(ParsedPath {
                path: String::new(),
                remainder: String::new(),
            });
            assert_eq!(parsed.path, address, "must survive verbatim");
        }
    }

    #[test]
    fn reports_an_unclosed_bracket() {
        assert_eq!(
            parse_path("<alice@example.test"),
            Err(PathError::UnclosedBracket)
        );
    }

    #[test]
    fn reports_a_missing_address() {
        assert_eq!(parse_path(""), Err(PathError::MissingOpenBracket));
        assert_eq!(parse_path("   "), Err(PathError::MissingOpenBracket));
    }

    #[test]
    fn does_not_validate_semantics() {
        // Transport syntax only: these are nonsense addresses that must still
        // parse, so the domain layer decides what to do with them.
        for weird in ["@", "no-at-sign", "a@b@c", "  spaces  "] {
            let parsed = parse_path(&format!("<{weird}>"));
            assert!(parsed.is_ok(), "{weird} must be delimited, not judged");
        }
    }
}
