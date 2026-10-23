//! Per-connection protocol transcripts (ADR-066).
//!
//! A [`Transcript`] is a shared, bounded, redacting buffer. The listener makes
//! one per **accepted socket** and hands a clone to every [`Connection`] built
//! over it, so a `STARTTLS` upgrade — which deliberately discards the session
//! and builds a second `Connection` (ADR-036) — records as one continuous
//! conversation rather than two unrelated fragments.
//!
//! [`Connection`]: crate::Connection
//!
//! # Redaction happens here, at capture
//!
//! Credentials and message bodies are never written into the buffer. They are
//! not stored-then-hidden, because a buffer that has held a password has
//! already lost: it can be dumped by a panic, a debugger, or a future feature
//! that renders "the raw transcript".
//!
//! `crates/sandesh-app/src/logging.rs` states the underlying rule — a developer
//! mail server holds password-reset links and one-time codes, and a transcript
//! is a log. There is deliberately no configuration flag to disable redaction:
//! a flag that can leak an OTP is a flag that eventually will.
//!
//! # Bounded twice
//!
//! A line cap and a total byte cap. An IMAP session can stay open for days, and
//! an unbounded per-connection buffer is a memory leak with a protocol in front
//! of it. On hitting either cap the transcript stops recording and marks itself
//! truncated, so a reader can tell a quiet connection from a cut-off one.

use std::sync::{Arc, Mutex};

/// The default maximum number of lines kept per connection.
pub const DEFAULT_MAX_LINES: usize = 500;

/// The default maximum number of transcript bytes kept per connection.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// How many bytes of a long line are kept before it is elided.
const MAX_LINE_BYTES: usize = 512;

/// Which side of the conversation a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Sent by the client.
    In,
    /// Sent by the server.
    Out,
}

impl Direction {
    const fn marker(self) -> &'static str {
        match self {
            Self::In => "C:",
            Self::Out => "S:",
        }
    }
}

/// The shared transcript for one accepted socket.
///
/// Cheap to clone: every clone appends to the same buffer.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    inner: Option<Arc<Mutex<Buffer>>>,
}

#[derive(Debug)]
struct Buffer {
    lines: Vec<String>,
    bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    truncated: bool,
}

impl Transcript {
    /// A recording transcript with the default caps.
    #[must_use]
    pub fn recording() -> Self {
        Self::with_caps(DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
    }

    /// A recording transcript with explicit caps.
    #[must_use]
    pub fn with_caps(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            inner: Some(Arc::new(Mutex::new(Buffer {
                lines: Vec::new(),
                bytes: 0,
                max_lines,
                max_bytes,
                truncated: false,
            }))),
        }
    }

    /// A transcript that records nothing.
    ///
    /// The default, so a `Connection` built without one costs nothing and the
    /// existing tests that construct connections directly keep working.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { inner: None }
    }

    /// Whether this transcript is recording.
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        self.inner.is_some()
    }

    /// Records one protocol line, redacting it first.
    pub fn line(&self, direction: Direction, bytes: &[u8]) {
        let Some(inner) = &self.inner else { return };
        let rendered = redact(direction, bytes);
        push(inner, format!("{} {}", direction.marker(), rendered));
    }

    /// Records a note in place of content that is deliberately not captured.
    ///
    /// Used for `DATA` payloads and IMAP literals: the reader learns that
    /// content passed and how much, and never sees the content.
    pub fn elided(&self, direction: Direction, label: &str, byte_count: usize) {
        let Some(inner) = &self.inner else { return };
        push(
            inner,
            format!(
                "{} [{label}: {byte_count} bytes not recorded]",
                direction.marker()
            ),
        );
    }

    /// Records a server-side event that is not a protocol line — a `STARTTLS`
    /// upgrade, a timeout, a shutdown.
    pub fn note(&self, text: &str) {
        let Some(inner) = &self.inner else { return };
        push(inner, format!("*  {text}"));
    }

    /// The recorded lines and whether the transcript was truncated.
    #[must_use]
    pub fn snapshot(&self) -> (Vec<String>, bool) {
        let Some(inner) = &self.inner else {
            return (Vec::new(), false);
        };
        // A poisoned lock means another thread panicked mid-append. The
        // transcript is diagnostic, so recovering the buffer is strictly better
        // than propagating the panic into a live connection.
        let guard = inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard.lines.clone(), guard.truncated)
    }
}

fn push(inner: &Arc<Mutex<Buffer>>, line: String) {
    let mut guard = inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.truncated {
        return;
    }
    if guard.lines.len() >= guard.max_lines || guard.bytes + line.len() > guard.max_bytes {
        guard.truncated = true;
        guard
            .lines
            .push(String::from("*  [transcript truncated: cap reached]"));
        return;
    }
    guard.bytes += line.len();
    guard.lines.push(line);
}

/// Commands whose *argument* is a credential and must never be recorded.
///
/// Matched case-insensitively on the verb. POP3 `PASS`, SMTP/IMAP `AUTH`,
/// IMAP `LOGIN`. The IMAP form is `<tag> LOGIN user pass`, so the verb is not
/// always the first token — [`redact`] handles that.
const CREDENTIAL_VERBS: [&str; 4] = ["PASS", "AUTH", "AUTHENTICATE", "LOGIN"];

/// Renders one line for the transcript, removing anything secret.
///
/// Client lines are the dangerous direction: a credential only ever travels
/// inbound, either as a command argument or as a bare base64 SASL response.
/// Server lines are protocol replies and are recorded as-is, save for length.
fn redact(direction: Direction, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_end_matches(['\r', '\n']);

    if direction == Direction::Out {
        return truncate_line(trimmed);
    }

    // A continuation line in a SASL exchange is a bare base64 blob with no
    // verb at all — `AUTH PLAIN` on one line, the credential on the next. It
    // cannot be recognised by its content, so the safe reading of any line
    // that is plausibly base64 and carries no space is "possibly a secret".
    if looks_like_bare_sasl_response(trimmed) {
        return String::from("[redacted: possible SASL response]");
    }

    let mut tokens = trimmed.split(' ');
    let Some(first) = tokens.next() else {
        return String::new();
    };

    if is_credential_verb(first) {
        return format!("{first} [redacted]");
    }

    // IMAP puts a tag first: `a001 LOGIN alice hunter2`.
    if let Some(second) = tokens.next()
        && is_credential_verb(second)
    {
        return format!("{first} {second} [redacted]");
    }

    truncate_line(trimmed)
}

fn is_credential_verb(token: &str) -> bool {
    CREDENTIAL_VERBS
        .iter()
        .any(|verb| token.eq_ignore_ascii_case(verb))
}

/// Whether a line looks like a bare base64 SASL response.
///
/// A SASL continuation carries no verb — `AUTH PLAIN` on one line, the
/// credential alone on the next — so it can only be recognised by shape. The
/// test leans towards redaction, because recording a credential costs a
/// password while redacting a command costs one diagnostic line.
///
/// It cannot lean *too* far, though: a bare argument-less command is also a
/// single alphanumeric word. `STARTTLS`, `QUIT`, `CAPABILITY` and `NOOP` are
/// all "one word, no spaces", and blanking them would gut the transcript at
/// exactly the moments worth reading. So a candidate must additionally look
/// like encoded data rather than a protocol keyword: either explicitly padded
/// or non-base64-alphabet-free *and* mixed-case, which no SMTP, IMAP or POP3
/// verb is.
fn looks_like_bare_sasl_response(line: &str) -> bool {
    if line.len() < 8 || line.contains(' ') {
        return false;
    }
    if !line
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return false;
    }

    // `=` padding, `+`/`/`, or digits do not appear in a bare protocol verb.
    let has_base64_marker = line
        .bytes()
        .any(|b| matches!(b, b'=' | b'+' | b'/') || b.is_ascii_digit());
    // Protocol verbs are conventionally single-case; base64 of any real
    // credential almost never is.
    let mixed_case = line.bytes().any(|b| b.is_ascii_uppercase())
        && line.bytes().any(|b| b.is_ascii_lowercase());

    has_base64_marker || mixed_case
}

/// Caps a single line, so one enormous command cannot dominate the buffer.
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        return line.to_owned();
    }
    // Slice on a character boundary: `line` came from `from_utf8_lossy`, so it
    // is valid UTF-8, but a multi-byte character may straddle the cap.
    let mut end = MAX_LINE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} bytes total]", &line[..end], line.len())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn recorded(transcript: &Transcript) -> String {
        transcript.snapshot().0.join("\n")
    }

    #[test]
    fn a_disabled_transcript_records_nothing() {
        let transcript = Transcript::disabled();
        transcript.line(Direction::In, b"EHLO localhost");
        transcript.note("something happened");
        assert_eq!(transcript.snapshot(), (Vec::new(), false));
    }

    #[test]
    fn ordinary_commands_and_replies_are_recorded_verbatim() {
        let transcript = Transcript::recording();
        transcript.line(Direction::In, b"EHLO localhost\r\n");
        transcript.line(Direction::Out, b"250-sandesh.test\r\n");

        let text = recorded(&transcript);
        assert!(text.contains("C: EHLO localhost"), "{text}");
        assert!(text.contains("S: 250-sandesh.test"), "{text}");
        // The terminator is framing, not content.
        assert!(!text.contains('\r'), "{text}");
    }

    #[test]
    fn pop3_pass_argument_never_reaches_the_buffer() {
        let transcript = Transcript::recording();
        transcript.line(Direction::In, b"PASS hunter2\r\n");

        let text = recorded(&transcript);
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("PASS [redacted]"), "{text}");
    }

    #[test]
    fn imap_login_is_redacted_despite_its_leading_tag() {
        // `a001 LOGIN alice hunter2` — the verb is the *second* token, which a
        // naive first-token check would miss entirely.
        let transcript = Transcript::recording();
        transcript.line(Direction::In, b"a001 LOGIN alice hunter2\r\n");

        let text = recorded(&transcript);
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains("alice"), "the username goes too: {text}");
        assert!(text.contains("a001 LOGIN [redacted]"), "{text}");
    }

    #[test]
    fn sasl_initial_response_and_continuation_are_both_redacted() {
        let transcript = Transcript::recording();
        // Initial response on the AUTH line itself.
        transcript.line(Direction::In, b"AUTH PLAIN AGFsaWNlAGh1bnRlcjI=\r\n");
        // And the bare-blob continuation form, which carries no verb.
        transcript.line(Direction::In, b"AGFsaWNlAGh1bnRlcjI=\r\n");

        let text = recorded(&transcript);
        assert!(
            !text.contains("AGFsaWNlAGh1bnRlcjI="),
            "a base64 credential must never appear: {text}"
        );
    }

    #[test]
    fn case_does_not_defeat_redaction() {
        // SMTP verbs are case-insensitive, so `pass` must redact like `PASS`.
        let transcript = Transcript::recording();
        transcript.line(Direction::In, b"pass hunter2\r\n");
        transcript.line(Direction::In, b"a1 login alice hunter2\r\n");

        let text = recorded(&transcript);
        assert!(!text.contains("hunter2"), "{text}");
    }

    #[test]
    fn bare_protocol_verbs_survive_the_sasl_heuristic() {
        // Regression: `STARTTLS` is eight alphanumeric characters with no
        // space, which an over-eager "looks like base64" test redacts. Blanking
        // the commands that have no arguments would gut the transcript exactly
        // where a handshake needs reading.
        let transcript = Transcript::recording();
        for verb in [
            &b"STARTTLS"[..],
            &b"CAPABILITY"[..],
            &b"NAMESPACE"[..],
            &b"UNSELECT"[..],
        ] {
            transcript.line(Direction::In, verb);
        }

        let text = recorded(&transcript);
        for verb in ["STARTTLS", "CAPABILITY", "NAMESPACE", "UNSELECT"] {
            assert!(text.contains(verb), "{verb} must survive: {text}");
        }
    }

    #[test]
    fn a_server_line_that_looks_like_base64_is_still_recorded() {
        // Redaction targets the inbound direction: a server challenge is not a
        // secret, and blanking it would make a SASL exchange unreadable.
        let transcript = Transcript::recording();
        transcript.line(Direction::Out, b"334 VXNlcm5hbWU6\r\n");
        assert!(recorded(&transcript).contains("334 VXNlcm5hbWU6"));
    }

    #[test]
    fn elided_content_reports_its_size_and_nothing_else() {
        let transcript = Transcript::recording();
        transcript.elided(Direction::In, "DATA", 4096);

        let text = recorded(&transcript);
        assert!(text.contains("DATA: 4096 bytes not recorded"), "{text}");
    }

    #[test]
    fn the_line_cap_truncates_and_marks_itself() {
        let transcript = Transcript::with_caps(3, DEFAULT_MAX_BYTES);
        for _ in 0..10 {
            transcript.line(Direction::In, b"NOOP");
        }

        let (lines, truncated) = transcript.snapshot();
        assert!(truncated, "hitting the cap must set the flag");
        assert!(lines.len() <= 4, "3 lines plus the marker: {lines:?}");
        assert!(lines.last().is_some_and(|l| l.contains("truncated")));
    }

    #[test]
    fn the_byte_cap_truncates_independently_of_the_line_cap() {
        let transcript = Transcript::with_caps(DEFAULT_MAX_LINES, 64);
        for _ in 0..10 {
            transcript.line(Direction::In, b"NOOP with a reasonably long argument");
        }

        let (_, truncated) = transcript.snapshot();
        assert!(truncated, "the byte cap must bind even under the line cap");
    }

    #[test]
    fn one_enormous_line_cannot_dominate_the_buffer() {
        let transcript = Transcript::recording();
        let huge = format!("NOOP {}", "x".repeat(10_000));
        transcript.line(Direction::In, huge.as_bytes());

        let (lines, _) = transcript.snapshot();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].len() < 700,
            "the line must be capped: {}",
            lines[0].len()
        );
        assert!(lines[0].contains("10005 bytes total"), "{}", lines[0]);
    }

    #[test]
    fn a_multibyte_character_at_the_cap_does_not_panic() {
        // Slicing a String at a non-boundary byte index panics; a client can
        // choose the byte offset of a multi-byte character deliberately.
        let transcript = Transcript::recording();
        let line = format!("NOOP {}", "é".repeat(1000));
        transcript.line(Direction::In, line.as_bytes());
        assert_eq!(transcript.snapshot().0.len(), 1);
    }

    #[test]
    fn clones_share_one_buffer() {
        // The STARTTLS case: the second Connection gets a clone, and both must
        // append to the same conversation.
        let first = Transcript::recording();
        let second = first.clone();

        first.line(Direction::In, b"STARTTLS");
        second.note("TLS established");
        second.line(Direction::In, b"EHLO localhost");

        let (lines, _) = first.snapshot();
        assert_eq!(lines.len(), 3, "one continuous transcript: {lines:?}");
    }
}
