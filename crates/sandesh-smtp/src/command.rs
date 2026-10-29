//! SMTP command parsing (RFC 5321 §4.1).
//!
//! # Why not prefix matching
//!
//! The obvious implementation — `if line.starts_with("MAIL")` — is wrong.
//! `MAILBOX` starts with `MAIL`, and `RCPTFOO` starts with `RCPT`. The verb is
//! therefore split on its delimiter *first* and then compared whole.
//!
//! `MAIL FROM:` and `RCPT TO:` are special: their delimiter is a colon, not a
//! space, and RFC 5321 permits no space before it. Both forms are handled
//! explicitly rather than by guessing.
//!
//! # Bytes in, text out
//!
//! Commands arrive as bytes. The verb and its arguments must be ASCII to be
//! recognisable at all, so a line that is not valid UTF-8 is a syntax error —
//! not a panic, and not a lossy conversion that might match something.

use crate::address::{ParsedPath, parse_path};
use sandesh_proto::Secret;
use std::fmt;

/// A parsed SMTP command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `EHLO <domain>` — extended greeting.
    Ehlo(String),
    /// `HELO <domain>` — legacy greeting.
    Helo(String),
    /// `MAIL FROM:<reverse-path> [params]`.
    Mail(MailCommand),
    /// `RCPT TO:<forward-path> [params]`.
    Rcpt(RcptCommand),
    /// `DATA`.
    Data,
    /// `BDAT <octets> [LAST]` — a counted content chunk (RFC 3030).
    ///
    /// Distinct from [`Data`](Self::Data) rather than a parameter of it: the
    /// two framings are incompatible (ADR-088). `BDAT` ends at a declared
    /// length with no dot-stuffing and answers every chunk, where `DATA` ends
    /// at `<CRLF>.<CRLF>`, unstuffs, and answers once.
    Bdat(BdatCommand),
    /// `RSET`.
    Rset,
    /// `NOOP [text]`.
    Noop,
    /// `QUIT`.
    Quit,
    /// `VRFY <string>`.
    Vrfy(String),
    /// `EXPN <string>`.
    Expn(String),
    /// `HELP [command]`.
    Help(Option<String>),
    /// `AUTH <mechanism> [initial-response]` (RFC 4954).
    ///
    /// The initial response is optional: `AUTH PLAIN <base64>` completes in one
    /// round trip, while a bare `AUTH PLAIN` asks the server to prompt.
    Auth(AuthCommand),
    /// `STARTTLS` (RFC 3207).
    StartTls,
    /// A recognised verb Sandesh does not implement in this phase.
    ///
    /// Distinguished from an unknown verb so the reply can be `502 not
    /// implemented` rather than `500 unrecognised` — the difference tells a
    /// client whether to stop asking or to report a bug.
    Unimplemented(String),
}

/// A parsed `AUTH` command.
///
/// The mechanism name is kept as the client spelled it so an unsupported
/// mechanism can be named back in the error, which is what lets a developer
/// see that their client asked for CRAM-MD5 (ADR-032).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCommand {
    /// The mechanism the client asked for, uppercased.
    pub mechanism: String,
    /// The base64 initial response, when the client sent one.
    ///
    /// A [`Secret`] because for `PLAIN` this *is* the credential: it decodes
    /// straight to a password, so it must not reach a log even though it is
    /// still base64 at this point (ADR-035).
    pub initial_response: Option<Secret>,
}

/// A parsed `MAIL FROM` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailCommand {
    /// The reverse-path. `None` is the null sender `<>`, used by bounces.
    pub reverse_path: Option<String>,
    /// The `SIZE=` parameter, when the client declared one.
    pub declared_size: Option<usize>,
    /// The `BODY=` parameter, when present.
    pub body_type: Option<BodyType>,
}

/// A parsed `RCPT TO` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcptCommand {
    /// The forward-path. Never empty: `RCPT TO:<>` is invalid (RFC 5321 §4.1.1.3).
    pub forward_path: String,
}

/// The `BODY=` parameter of `MAIL FROM` (RFC 6152).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyType {
    /// `BODY=7BIT`.
    SevenBit,
    /// `BODY=8BITMIME`.
    EightBitMime,
}

/// Why a command line could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The line was empty.
    #[error("empty command")]
    Empty,

    /// The verb is not one Sandesh knows.
    #[error("unrecognised command")]
    UnknownCommand,

    /// The line contained bytes that cannot be a command.
    #[error("command contains non-ASCII bytes")]
    NotAscii,

    /// A `MAIL` or `RCPT` whose arguments are not ASCII: an internationalised
    /// address, which needs SMTPUTF8 (RFC 6531) and is not yet supported.
    #[error("non-ASCII address requires SMTPUTF8")]
    NeedsSmtpUtf8,

    /// The command's arguments were malformed.
    #[error("{0}")]
    Syntax(String),

    /// A parameter was well-formed but is not supported.
    #[error("{0}")]
    UnsupportedParameter(String),
}

impl Command {
    /// The verb, for logging and diagnostics.
    #[must_use]
    pub fn verb(&self) -> &str {
        match self {
            Self::Ehlo(_) => "EHLO",
            Self::Helo(_) => "HELO",
            Self::Mail(_) => "MAIL",
            Self::Rcpt(_) => "RCPT",
            Self::Data => "DATA",
            Self::Bdat(_) => "BDAT",
            Self::Rset => "RSET",
            Self::Noop => "NOOP",
            Self::Quit => "QUIT",
            Self::Vrfy(_) => "VRFY",
            Self::Expn(_) => "EXPN",
            Self::Help(_) => "HELP",
            // Only the verb, never the argument: for AUTH the argument is the
            // credential (ADR-035).
            Self::Auth(_) => "AUTH",
            Self::StartTls => "STARTTLS",
            Self::Unimplemented(verb) => verb,
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.verb())
    }
}

/// Parses one command line, which must already have its CRLF removed.
///
/// # Errors
/// Returns [`ParseError`] describing what was wrong, so the session can choose
/// between `500`, `501`, `502` and `504`.
pub fn parse(line: &[u8]) -> Result<Command, ParseError> {
    // The command line must be ASCII. Message *content* may be arbitrary bytes,
    // but a command that is not ASCII cannot name any verb.
    if !line.is_ascii() {
        // The verb is ASCII even when the address is not, so a client sending
        // `MAIL FROM:<jöran@…>` can be told what is actually wrong.
        let verb = line.get(..4).unwrap_or_default();
        let separated = line.get(4).is_some_and(|b| *b == b' ' || *b == b':');
        if separated && (verb.eq_ignore_ascii_case(b"MAIL") || verb.eq_ignore_ascii_case(b"RCPT")) {
            return Err(ParseError::NeedsSmtpUtf8);
        }
        return Err(ParseError::NotAscii);
    }
    let line = core::str::from_utf8(line).map_err(|_| ParseError::NotAscii)?;

    let trimmed = line.trim_end_matches([' ', '\t']);
    if trimmed.trim().is_empty() {
        return Err(ParseError::Empty);
    }

    // Split the verb from its arguments. `MAIL FROM:` and `RCPT TO:` use a
    // colon, everything else a space — so both delimiters are considered and
    // the earlier one wins.
    let (verb, rest) = split_verb(trimmed);

    match verb.to_ascii_uppercase().as_str() {
        "EHLO" => parse_greeting(rest).map(Command::Ehlo),
        "HELO" => parse_greeting(rest).map(Command::Helo),
        "MAIL" => parse_mail(rest).map(Command::Mail),
        "RCPT" => parse_rcpt(rest).map(Command::Rcpt),
        "DATA" => expect_no_args(rest, Command::Data),
        "RSET" => expect_no_args(rest, Command::Rset),
        // NOOP may carry an ignored argument (RFC 5321 §4.1.1.9).
        "NOOP" => Ok(Command::Noop),
        "QUIT" => expect_no_args(rest, Command::Quit),
        "VRFY" => {
            if rest.trim().is_empty() {
                Err(ParseError::Syntax(String::from(
                    "VRFY requires an argument",
                )))
            } else {
                Ok(Command::Vrfy(rest.trim().to_owned()))
            }
        }
        "EXPN" => {
            if rest.trim().is_empty() {
                Err(ParseError::Syntax(String::from(
                    "EXPN requires an argument",
                )))
            } else {
                Ok(Command::Expn(rest.trim().to_owned()))
            }
        }
        "HELP" => Ok(Command::Help({
            let arg = rest.trim();
            if arg.is_empty() {
                None
            } else {
                Some(arg.to_owned())
            }
        })),
        // Recognised verbs Sandesh does not implement in this phase. Naming
        // them earns a `502` instead of a misleading `500`.
        "AUTH" => parse_auth(rest),
        // RFC 3207 §4: STARTTLS takes no parameters, and a server must reject
        // one rather than ignore it.
        "STARTTLS" => {
            if rest.trim().is_empty() {
                Ok(Command::StartTls)
            } else {
                Err(ParseError::Syntax(String::from(
                    "STARTTLS takes no parameters",
                )))
            }
        }
        // BDAT left this group in Phase 8 A4 (ADR-088). The rest stay:
        // naming them earns a truthful `502` rather than a misleading `500`.
        "BDAT" => parse_bdat(rest),
        verb @ ("ETRN" | "TURN" | "ATRN" | "SEND" | "SOML" | "SAML") => {
            Ok(Command::Unimplemented(verb.to_owned()))
        }
        _ => Err(ParseError::UnknownCommand),
    }
}

/// A parsed `BDAT` command (RFC 3030 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BdatCommand {
    /// How many octets of content follow the CRLF, exactly.
    ///
    /// Not validated against the message limit here: that is a policy question
    /// the session answers, and the parser's job is to report what the client
    /// said. The session checks it before allocating (ADR-088).
    pub size: usize,
    /// Whether this is the final chunk — `BDAT <n> LAST`.
    pub last: bool,
}

/// Parses `BDAT <octets> [LAST]` (RFC 3030 §2).
///
/// The size is mandatory and must be a bare decimal count. A missing or
/// unparseable size is a syntax error rather than a default, because guessing
/// would mean reading an unknown number of octets as commands.
fn parse_bdat(rest: &str) -> Result<Command, ParseError> {
    let mut parts = rest.split_whitespace();

    let Some(size) = parts.next() else {
        return Err(ParseError::Syntax(String::from(
            "BDAT requires a chunk size",
        )));
    };

    let size = size.parse::<usize>().map_err(|_| {
        ParseError::Syntax(format!(
            "BDAT size must be a decimal octet count, got {size:?}"
        ))
    })?;

    // RFC 3030 §2: the only permitted second argument is the literal `LAST`.
    let last = match parts.next() {
        None => false,
        Some(word) if word.eq_ignore_ascii_case("LAST") => true,
        Some(word) => {
            return Err(ParseError::Syntax(format!(
                "BDAT takes an optional LAST, got {word:?}"
            )));
        }
    };

    if parts.next().is_some() {
        return Err(ParseError::Syntax(String::from(
            "BDAT takes at most two parameters",
        )));
    }

    Ok(Command::Bdat(BdatCommand { size, last }))
}

/// Parses the arguments of an `AUTH` command (RFC 4954 §4).
///
/// `AUTH <mechanism>` prompts; `AUTH <mechanism> <base64>` supplies an initial
/// response and saves a round trip. A missing mechanism is a syntax error
/// rather than a silent default.
fn parse_auth(rest: &str) -> Result<Command, ParseError> {
    let mut parts = rest.split_whitespace();

    let Some(mechanism) = parts.next() else {
        return Err(ParseError::Syntax(String::from(
            "AUTH requires a mechanism",
        )));
    };

    let initial_response = parts.next().map(Secret::new);

    // RFC 4954 permits at most one initial response.
    if parts.next().is_some() {
        return Err(ParseError::Syntax(String::from(
            "AUTH takes a mechanism and at most one initial response",
        )));
    }

    Ok(Command::Auth(AuthCommand {
        mechanism: mechanism.to_ascii_uppercase(),
        initial_response,
    }))
}

/// Splits the verb from the remainder.
///
/// Considers both the space and the colon, taking whichever comes first, so
/// `MAIL FROM:<a@b>` yields `("MAIL", "FROM:<a@b>")` while `MAILBOX` yields
/// `("MAILBOX", "")` — which then fails to match any verb, as it must.
fn split_verb(line: &str) -> (&str, &str) {
    let space = line.find(' ');
    let colon = line.find(':');

    match (space, colon) {
        (Some(s), Some(c)) if s < c => (&line[..s], &line[s + 1..]),
        (Some(s), None) => (&line[..s], &line[s + 1..]),
        // A colon before any space: the verb ends at the colon, which is kept
        // in the remainder so `MAIL FROM:` style parsing sees it.
        (Some(_) | None, Some(c)) => (&line[..c], &line[c..]),
        (None, None) => (line, ""),
    }
}

fn expect_no_args(rest: &str, command: Command) -> Result<Command, ParseError> {
    if rest.trim().is_empty() {
        Ok(command)
    } else {
        Err(ParseError::Syntax(format!("{command} takes no arguments")))
    }
}

/// Parses the domain argument of `EHLO`/`HELO`.
///
/// Deliberately permissive: RFC 5321 §4.1.1.1 requires a domain or address
/// literal, but a development mail server that refuses `EHLO localhost` from a
/// test harness is failing at its purpose. Only an empty argument is rejected.
fn parse_greeting(rest: &str) -> Result<String, ParseError> {
    let domain = rest.trim();
    if domain.is_empty() {
        return Err(ParseError::Syntax(String::from(
            "EHLO/HELO requires a domain argument",
        )));
    }
    // Take the first token: some clients append extra whitespace-separated text.
    Ok(domain
        .split_whitespace()
        .next()
        .unwrap_or(domain)
        .to_owned())
}

/// Parses `FROM:<reverse-path> [SIZE=n] [BODY=...]`.
fn parse_mail(rest: &str) -> Result<MailCommand, ParseError> {
    let rest = rest.trim_start();
    // The keyword is `FROM`, and RFC 5321 permits no space before the colon.
    let after_keyword = strip_keyword(rest, "FROM")
        .ok_or_else(|| ParseError::Syntax(String::from("expected MAIL FROM:<reverse-path>")))?;

    let ParsedPath { path, remainder } = parse_path(after_keyword)
        .map_err(|e| ParseError::Syntax(format!("invalid reverse-path: {e}")))?;

    let mut command = MailCommand {
        // An empty path is the null reverse-path `<>`, which is valid and is
        // how bounce messages identify themselves (RFC 5321 §4.5.5).
        reverse_path: if path.is_empty() { None } else { Some(path) },
        declared_size: None,
        body_type: None,
    };

    for parameter in remainder.split_whitespace() {
        let (key, value) = parameter
            .split_once('=')
            .map_or((parameter, ""), |(k, v)| (k, v));

        match key.to_ascii_uppercase().as_str() {
            // Advertised via SIZE, so it must be honoured (RFC 1870).
            "SIZE" => {
                command.declared_size = Some(value.parse().map_err(|_| {
                    ParseError::Syntax(format!("SIZE must be a number, got {value:?}"))
                })?);
            }
            // Advertised via 8BITMIME (RFC 6152). Both values are accepted:
            // Sandesh stores raw bytes either way, so the declaration changes
            // nothing about handling — it only needs to not be rejected.
            "BODY" => {
                command.body_type = Some(match value.to_ascii_uppercase().as_str() {
                    "7BIT" => BodyType::SevenBit,
                    "8BITMIME" => BodyType::EightBitMime,
                    other => {
                        return Err(ParseError::UnsupportedParameter(format!(
                            "BODY={other} is not supported; use 7BIT or 8BITMIME"
                        )));
                    }
                });
            }
            // SMTPUTF8 is Phase 7 and is not advertised, so a client should not
            // send it. Naming it explicitly gives a better answer than a
            // generic one.
            "SMTPUTF8" => {
                return Err(ParseError::UnsupportedParameter(String::from(
                    "SMTPUTF8 is not supported",
                )));
            }
            other => {
                return Err(ParseError::UnsupportedParameter(format!(
                    "unsupported parameter {other}"
                )));
            }
        }
    }

    Ok(command)
}

/// Parses `TO:<forward-path>`.
fn parse_rcpt(rest: &str) -> Result<RcptCommand, ParseError> {
    let rest = rest.trim_start();
    let after_keyword = strip_keyword(rest, "TO")
        .ok_or_else(|| ParseError::Syntax(String::from("expected RCPT TO:<forward-path>")))?;

    let ParsedPath { path, remainder } = parse_path(after_keyword)
        .map_err(|e| ParseError::Syntax(format!("invalid forward-path: {e}")))?;

    if path.is_empty() {
        // Unlike MAIL FROM, an empty forward-path is meaningless.
        return Err(ParseError::Syntax(String::from(
            "RCPT TO requires a recipient address",
        )));
    }

    // RFC 5321 §4.1.1.3 defines DSN parameters here; none are advertised, so a
    // client sending one gets a clear answer rather than silent acceptance.
    if !remainder.trim().is_empty() {
        return Err(ParseError::UnsupportedParameter(format!(
            "unsupported RCPT parameter: {}",
            remainder.trim()
        )));
    }

    Ok(RcptCommand { forward_path: path })
}

/// Strips a `KEYWORD:` prefix, case-insensitively.
///
/// Returns `None` when the keyword is absent, so `MAIL TO:<x>` is rejected
/// rather than silently treated as `MAIL FROM`.
fn strip_keyword<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let input = input.trim_start();
    if input.len() < keyword.len() {
        return None;
    }
    if !input[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    // Tolerate whitespace before the colon: RFC 5321 forbids it, but clients
    // send it, and Sandesh exists to accept mail rather than to police senders.
    let after = input[keyword.len()..].trim_start();
    after.strip_prefix(':')
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn cmd(line: &str) -> Result<Command, ParseError> {
        parse(line.as_bytes())
    }

    #[test]
    fn verbs_are_case_insensitive() {
        for spelling in ["EHLO", "ehlo", "EhLo", "eHLo"] {
            let parsed = cmd(&format!("{spelling} example.test"));
            assert!(
                matches!(parsed, Ok(Command::Ehlo(_))),
                "{spelling} must parse"
            );
        }
    }

    #[test]
    fn mail_and_rcpt_keywords_are_case_insensitive() {
        assert!(matches!(cmd("MAIL from:<a@b.test>"), Ok(Command::Mail(_))));
        assert!(matches!(cmd("rCpT To:<a@b.test>"), Ok(Command::Rcpt(_))));
    }

    #[test]
    fn similar_verbs_are_not_confused() {
        // The prefix-matching bug this parser exists to avoid.
        for line in ["MAILBOX", "RCPTFOO", "DATABASE", "QUITE", "NOOPS", "HELOX"] {
            assert!(
                matches!(cmd(line), Err(ParseError::UnknownCommand)),
                "{line} must not be mistaken for a known verb"
            );
        }
    }

    #[test]
    fn parses_a_plain_sender() {
        let Ok(Command::Mail(mail)) = cmd("MAIL FROM:<alice@example.test>") else {
            panic!("expected MAIL");
        };
        assert_eq!(mail.reverse_path.as_deref(), Some("alice@example.test"));
        assert_eq!(mail.declared_size, None);
    }

    #[test]
    fn parses_the_null_reverse_path() {
        // Bounce messages depend on this being accepted.
        let Ok(Command::Mail(mail)) = cmd("MAIL FROM:<>") else {
            panic!("expected MAIL");
        };
        assert_eq!(mail.reverse_path, None);
    }

    #[test]
    fn parses_size_and_body_parameters() {
        let Ok(Command::Mail(mail)) = cmd("MAIL FROM:<a@b.test> SIZE=4096 BODY=8BITMIME") else {
            panic!("expected MAIL");
        };
        assert_eq!(mail.declared_size, Some(4096));
        assert_eq!(mail.body_type, Some(BodyType::EightBitMime));
    }

    #[test]
    fn rejects_unadvertised_parameters() {
        // Never accept a parameter whose capability is not advertised.
        assert!(matches!(
            cmd("MAIL FROM:<a@b.test> SMTPUTF8"),
            Err(ParseError::UnsupportedParameter(_))
        ));
        assert!(matches!(
            cmd("MAIL FROM:<a@b.test> RET=FULL"),
            Err(ParseError::UnsupportedParameter(_))
        ));
    }

    #[test]
    fn rejects_a_malformed_size() {
        assert!(matches!(
            cmd("MAIL FROM:<a@b.test> SIZE=abc"),
            Err(ParseError::Syntax(_))
        ));
    }

    #[test]
    fn parses_a_recipient() {
        let Ok(Command::Rcpt(rcpt)) = cmd("RCPT TO:<bob@example.test>") else {
            panic!("expected RCPT");
        };
        assert_eq!(rcpt.forward_path, "bob@example.test");
    }

    #[test]
    fn rejects_an_empty_recipient() {
        // RFC 5321 §4.1.1.3: unlike MAIL FROM, RCPT TO:<> is meaningless.
        assert!(matches!(cmd("RCPT TO:<>"), Err(ParseError::Syntax(_))));
    }

    #[test]
    fn rejects_the_wrong_keyword() {
        assert!(matches!(
            cmd("MAIL TO:<a@b.test>"),
            Err(ParseError::Syntax(_))
        ));
        assert!(matches!(
            cmd("RCPT FROM:<a@b.test>"),
            Err(ParseError::Syntax(_))
        ));
    }

    #[test]
    fn argument_free_commands_reject_arguments() {
        for line in ["DATA now", "QUIT please", "RSET all"] {
            assert!(
                matches!(cmd(line), Err(ParseError::Syntax(_))),
                "{line} must be rejected"
            );
        }
    }

    #[test]
    fn noop_accepts_an_ignored_argument() {
        // RFC 5321 §4.1.1.9.
        assert_eq!(cmd("NOOP"), Ok(Command::Noop));
        assert_eq!(cmd("NOOP keepalive"), Ok(Command::Noop));
    }

    #[test]
    fn unimplemented_verbs_are_distinguished_from_unknown_ones() {
        // The difference decides between 502 and 500. AUTH and STARTTLS moved
        // out of this set in Phase 4.5, and BDAT in Phase 8 A4 (ADR-088) — each
        // is now a parsed command, so a 502 for it would be a lie. That is the
        // rule this test protects, and membership changes only when an
        // implementation lands.
        assert!(matches!(
            cmd("BDAT 10"),
            Ok(Command::Bdat(BdatCommand {
                size: 10,
                last: false
            }))
        ));
        assert!(matches!(
            cmd("ETRN example.test"),
            Ok(Command::Unimplemented(_))
        ));
        assert!(matches!(cmd("FLURB"), Err(ParseError::UnknownCommand)));
    }

    #[test]
    fn bdat_parses_its_size_and_last_marker() {
        assert_eq!(
            cmd("BDAT 42"),
            Ok(Command::Bdat(BdatCommand {
                size: 42,
                last: false
            }))
        );
        assert_eq!(
            cmd("BDAT 42 LAST"),
            Ok(Command::Bdat(BdatCommand {
                size: 42,
                last: true
            }))
        );
        // RFC 3030 §2 spells it `LAST`; clients vary in case.
        assert_eq!(
            cmd("BDAT 0 last"),
            Ok(Command::Bdat(BdatCommand {
                size: 0,
                last: true
            }))
        );
    }

    #[test]
    fn a_bad_bdat_size_is_refused_rather_than_defaulted() {
        // The dangerous failure: defaulting would mean reading an unknown
        // number of octets as commands.
        for bad in ["BDAT", "BDAT abc", "BDAT -1", "BDAT 1.5"] {
            assert!(
                matches!(cmd(bad), Err(ParseError::Syntax(_))),
                "{bad:?} must be a syntax error"
            );
        }
    }

    #[test]
    fn bdat_rejects_anything_but_last_as_its_second_argument() {
        for bad in ["BDAT 5 SOON", "BDAT 5 LAST EXTRA"] {
            assert!(
                matches!(cmd(bad), Err(ParseError::Syntax(_))),
                "{bad:?} must be a syntax error"
            );
        }
    }

    #[test]
    fn auth_is_parsed_with_and_without_an_initial_response() {
        // `AUTH PLAIN <base64>` completes in one round trip; a bare
        // `AUTH PLAIN` asks the server to prompt.
        let Ok(Command::Auth(prompted)) = cmd("AUTH PLAIN") else {
            panic!("AUTH PLAIN must parse");
        };
        assert_eq!(prompted.mechanism, "PLAIN");
        assert!(prompted.initial_response.is_none());

        let Ok(Command::Auth(inline)) = cmd("AUTH PLAIN AGRldgBodW50ZXIy") else {
            panic!("AUTH with an initial response must parse");
        };
        assert_eq!(inline.mechanism, "PLAIN");
        assert!(inline.initial_response.is_some());
    }

    #[test]
    fn the_auth_mechanism_is_uppercased_but_unknown_ones_still_parse() {
        // Parsing must accept any mechanism name so the session can answer
        // `504` naming it — including CRAM-MD5, which is refused on purpose
        // (ADR-032) rather than treated as a syntax error.
        let Ok(Command::Auth(auth)) = cmd("auth cram-md5") else {
            panic!("an unsupported mechanism must still parse");
        };
        assert_eq!(auth.mechanism, "CRAM-MD5");
    }

    #[test]
    fn an_auth_credential_never_reaches_a_log_through_the_command() {
        // The command is held in the session and logged on error paths, so the
        // initial response must be a `Secret` (ADR-035).
        let Ok(command) = cmd("AUTH PLAIN AGRldgBodW50ZXIy") else {
            panic!("parses");
        };
        let rendered = format!("{command:?}");
        assert!(
            !rendered.contains("AGRldgBodW50ZXIy"),
            "the credential leaked: {rendered}"
        );
        // The verb is still identifiable for diagnostics.
        assert_eq!(command.verb(), "AUTH");
    }

    #[test]
    fn auth_without_a_mechanism_is_a_syntax_error() {
        assert!(matches!(cmd("AUTH"), Err(ParseError::Syntax(_))));
        assert!(matches!(cmd("AUTH   "), Err(ParseError::Syntax(_))));
    }

    #[test]
    fn auth_with_too_many_arguments_is_rejected() {
        assert!(matches!(
            cmd("AUTH PLAIN abc def"),
            Err(ParseError::Syntax(_))
        ));
    }

    #[test]
    fn starttls_takes_no_parameters() {
        // RFC 3207 §4: a parameter must be rejected, not ignored.
        assert!(matches!(cmd("STARTTLS"), Ok(Command::StartTls)));
        assert!(matches!(cmd("starttls"), Ok(Command::StartTls)));
        assert!(matches!(cmd("STARTTLS now"), Err(ParseError::Syntax(_))));
    }

    #[test]
    fn empty_input_is_reported_as_empty() {
        assert_eq!(cmd(""), Err(ParseError::Empty));
        assert_eq!(cmd("   "), Err(ParseError::Empty));
    }

    #[test]
    fn non_ascii_command_lines_are_rejected_not_panicked() {
        assert_eq!(parse(b"EHLO \xff\xfe"), Err(ParseError::NotAscii));
        assert_eq!(parse(&[0xC3, 0x28]), Err(ParseError::NotAscii));
    }

    #[test]
    fn a_non_ascii_address_is_reported_as_needing_smtputf8() {
        for line in ["MAIL FROM:<jöran@app.test>", "rcpt TO:<jöran@app.test>"] {
            assert_eq!(
                parse(line.as_bytes()),
                Err(ParseError::NeedsSmtpUtf8),
                "{line}"
            );
        }
        // Only the address verbs: a non-ASCII byte elsewhere is still noise.
        assert_eq!(parse("MAILX ö".as_bytes()), Err(ParseError::NotAscii));
        assert_eq!(parse("VRFY jöran".as_bytes()), Err(ParseError::NotAscii));
    }

    #[test]
    fn tolerates_extra_whitespace() {
        assert!(matches!(
            cmd("MAIL  FROM:<a@b.test>   "),
            Ok(Command::Mail(_))
        ));
        assert!(matches!(cmd("EHLO   example.test  "), Ok(Command::Ehlo(_))));
    }

    #[test]
    fn greeting_requires_a_domain() {
        assert!(matches!(cmd("EHLO"), Err(ParseError::Syntax(_))));
        assert!(matches!(cmd("HELO   "), Err(ParseError::Syntax(_))));
    }

    #[test]
    fn vrfy_and_expn_require_arguments() {
        assert!(matches!(cmd("VRFY"), Err(ParseError::Syntax(_))));
        assert!(matches!(cmd("EXPN"), Err(ParseError::Syntax(_))));
        assert!(matches!(cmd("VRFY alice"), Ok(Command::Vrfy(_))));
    }

    #[test]
    fn help_is_optional_argument() {
        assert_eq!(cmd("HELP"), Ok(Command::Help(None)));
        assert_eq!(
            cmd("HELP MAIL"),
            Ok(Command::Help(Some(String::from("MAIL"))))
        );
    }
}
