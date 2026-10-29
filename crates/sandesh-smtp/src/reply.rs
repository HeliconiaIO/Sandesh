//! SMTP reply codes and their wire representation (RFC 5321 §4.2).
//!
//! Replies are built through this module rather than formatted at call sites,
//! so every response is well-formed and the choice of code is a visible,
//! reviewable decision. Returning `500` for everything is the classic failure
//! mode of a hand-rolled SMTP server, and centralising the vocabulary is what
//! prevents it.

use std::fmt::Write as _;

/// A three-digit SMTP reply code.
///
/// Only codes Sandesh actually emits are named. An unnamed code is a code
/// nobody decided to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Code {
    /// Service ready — the connection greeting.
    ServiceReady = 220,
    /// Service closing transmission channel.
    ServiceClosing = 221,
    /// Requested action completed.
    Ok = 250,
    /// Cannot verify the user, but will accept the message (RFC 5321 §3.5.3).
    CannotVerify = 252,
    /// Start mail input; end with `<CRLF>.<CRLF>`.
    StartMailInput = 354,
    /// Service not available, closing channel.
    ServiceUnavailable = 421,
    /// Requested action not taken: mailbox unavailable, try again.
    MailboxUnavailable = 450,
    /// Requested action aborted: local error in processing.
    LocalError = 451,
    /// Requested action not taken: insufficient system storage.
    InsufficientStorage = 452,
    /// Syntax error, command unrecognised.
    SyntaxError = 500,
    /// Syntax error in parameters or arguments.
    ParameterSyntaxError = 501,
    /// Command not implemented.
    NotImplemented = 502,
    /// Bad sequence of commands.
    BadSequence = 503,
    /// Command parameter not implemented.
    ParameterNotImplemented = 504,
    /// Requested action not taken: mailbox unavailable (permanent).
    MailboxUnavailablePermanent = 550,
    /// Transaction failed.
    TransactionFailed = 554,
    /// Message exceeds the size limit.
    MessageTooLarge = 552,
    /// Requested action not taken: mailbox name not allowed.
    MailboxNameNotAllowed = 553,

    // --- RFC 4954 (AUTH) and RFC 3207 (STARTTLS) ---
    /// Server challenge during a SASL exchange.
    AuthChallenge = 334,
    /// Authentication succeeded.
    AuthSucceeded = 235,
    /// Authentication credentials invalid.
    AuthInvalid = 535,
    /// Authentication required before this command.
    AuthRequired = 530,
    /// A stronger mechanism, or encryption, is required first.
    EncryptionRequired = 538,
    /// The SASL exchange failed for a reason other than bad credentials.
    AuthFailed = 454,
}

impl Code {
    /// The numeric value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self as u16
    }

    /// Whether this code closes the connection.
    #[must_use]
    pub const fn is_closing(self) -> bool {
        matches!(self, Self::ServiceClosing | Self::ServiceUnavailable)
    }

    /// Whether this is a positive completion or intermediate reply.
    #[must_use]
    pub const fn is_positive(self) -> bool {
        (self as u16) < 400
    }
}

/// An SMTP reply: a code, an optional enhanced status code, and text.
///
/// Enhanced status codes (RFC 3463) are included because `ENHANCEDSTATUSCODES`
/// is advertised, and advertising it without emitting them would be exactly the
/// dishonesty this module exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    code: Code,
    enhanced: Option<&'static str>,
    lines: Vec<String>,
}

impl Reply {
    /// A single-line reply.
    #[must_use]
    pub fn new(code: Code, text: impl Into<String>) -> Self {
        Self {
            code,
            enhanced: None,
            lines: vec![text.into()],
        }
    }

    /// A single-line reply carrying an enhanced status code.
    #[must_use]
    pub fn enhanced(code: Code, status: &'static str, text: impl Into<String>) -> Self {
        Self {
            code,
            enhanced: Some(status),
            lines: vec![text.into()],
        }
    }

    /// A multi-line reply, used for the `EHLO` capability list.
    #[must_use]
    pub fn multiline(code: Code, lines: Vec<String>) -> Self {
        Self {
            code,
            enhanced: None,
            lines: if lines.is_empty() {
                vec![String::new()]
            } else {
                lines
            },
        }
    }

    /// The reply code.
    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    /// Whether sending this reply should close the connection.
    #[must_use]
    pub const fn is_closing(&self) -> bool {
        self.code.is_closing()
    }

    /// Serialises to wire format.
    ///
    /// Continuation lines use `250-`; the final line uses `250 ` (RFC 5321
    /// §4.2.1). Getting that separator wrong is the classic multi-line `EHLO`
    /// bug: a client either hangs waiting for more or truncates the list.
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut out = String::with_capacity(self.lines.len() * 32);
        let last = self.lines.len().saturating_sub(1);

        for (index, line) in self.lines.iter().enumerate() {
            let separator = if index == last { ' ' } else { '-' };
            // Only the final line of a multi-line reply carries the enhanced
            // status code, per RFC 2034 §4.
            let status = match (index == last, self.enhanced) {
                (true, Some(status)) => format!("{status} "),
                _ => String::new(),
            };
            let _ = writeln!(out, "{}{separator}{status}{line}\r", self.code.value());
        }
        out
    }
}

// --- The replies Sandesh sends. Naming them keeps the vocabulary reviewable. ---

/// `220` greeting.
#[must_use]
pub fn greeting(hostname: &str) -> Reply {
    Reply::new(
        Code::ServiceReady,
        format!("{hostname} Sandesh ESMTP ready"),
    )
}

/// `221` closing.
#[must_use]
pub fn closing(hostname: &str) -> Reply {
    Reply::enhanced(
        Code::ServiceClosing,
        "2.0.0",
        format!("{hostname} closing connection"),
    )
}

/// `250 OK`.
#[must_use]
pub fn ok(text: impl Into<String>) -> Reply {
    Reply::enhanced(Code::Ok, "2.0.0", text)
}

/// `334` carrying a base64 SASL challenge.
///
/// The payload is the prompt, already base64-encoded.
#[must_use]
pub fn auth_challenge(encoded: impl Into<String>) -> Reply {
    Reply::new(Code::AuthChallenge, encoded)
}

/// `235` on successful authentication.
#[must_use]
pub fn auth_succeeded() -> Reply {
    Reply::enhanced(Code::AuthSucceeded, "2.7.0", "Authentication succeeded")
}

/// `535` for an invalid credential.
///
/// Deliberately says nothing about *which* part was wrong. Distinguishing an
/// unknown user from a bad password would turn every failure into a username
/// oracle, which is the same leak the constant-time verifier exists to close.
#[must_use]
pub fn auth_invalid() -> Reply {
    Reply::enhanced(
        Code::AuthInvalid,
        "5.7.8",
        "Authentication credentials invalid",
    )
}

/// `530` when a command requires authentication that has not happened.
#[must_use]
pub fn auth_required() -> Reply {
    Reply::enhanced(Code::AuthRequired, "5.7.0", "Authentication required")
}

/// `538` when `AUTH` is attempted without encryption.
///
/// `PLAIN` and `LOGIN` both send the password, so they are refused in the clear
/// (ADR-033). The text names the fix, because a client author needs to know
/// this is a policy and not a defect.
#[must_use]
pub fn encryption_required() -> Reply {
    Reply::enhanced(
        Code::EncryptionRequired,
        "5.7.11",
        "Encryption required for this mechanism; issue STARTTLS first",
    )
}

/// `501` when a client cancels a SASL exchange with `*` (RFC 4954 §4).
///
/// Distinct from an authentication failure: the client chose to abandon the
/// exchange, and reporting a bad credential would be untrue.
#[must_use]
pub fn auth_cancelled() -> Reply {
    Reply::enhanced(
        Code::ParameterSyntaxError,
        "5.5.2",
        "Authentication cancelled",
    )
}

/// `454` when a SASL exchange fails for a reason other than bad credentials.
#[must_use]
pub fn auth_failed(text: impl Into<String>) -> Reply {
    Reply::enhanced(Code::AuthFailed, "4.7.0", text)
}

/// `504` for a mechanism Sandesh does not implement.
///
/// Names the mechanism so a developer can see that their client asked for
/// something deliberately unsupported, such as CRAM-MD5 (ADR-032).
#[must_use]
pub fn unsupported_mechanism(mechanism: &str) -> Reply {
    Reply::enhanced(
        Code::ParameterNotImplemented,
        "5.5.4",
        format!("Unsupported authentication mechanism: {mechanism}"),
    )
}

/// `220` inviting a TLS handshake (RFC 3207 §4).
#[must_use]
pub fn ready_to_start_tls() -> Reply {
    Reply::new(Code::ServiceReady, "Ready to start TLS")
}

/// `250` acknowledging a sender.
#[must_use]
pub fn sender_ok() -> Reply {
    Reply::enhanced(Code::Ok, "2.1.0", "Sender OK")
}

/// `250` acknowledging a recipient.
#[must_use]
pub fn recipient_ok() -> Reply {
    Reply::enhanced(Code::Ok, "2.1.5", "Recipient OK")
}

/// `354` inviting message content.
#[must_use]
pub fn start_mail_input() -> Reply {
    Reply::new(
        Code::StartMailInput,
        "Start mail input; end with <CRLF>.<CRLF>",
    )
}

/// `250` acknowledging one accepted `BDAT` chunk (RFC 3030 §2).
///
/// Every chunk is answered, not only the last: that is the whole reason
/// `CHUNKING` can stream, since a client learns a chunk landed without waiting
/// for the transaction to finish. The octet count is echoed because a client
/// comparing it against what it sent is how a framing bug is caught early.
#[must_use]
pub fn chunk_accepted(octets: usize) -> Reply {
    Reply::enhanced(Code::Ok, "2.0.0", format!("{octets} octets received"))
}

/// `250` confirming a delivered message.
#[must_use]
pub fn message_accepted(id: &str) -> Reply {
    Reply::enhanced(Code::Ok, "2.6.0", format!("Message accepted: {id}"))
}

/// `500` unrecognised command.
#[must_use]
pub fn unrecognised_command() -> Reply {
    Reply::enhanced(Code::SyntaxError, "5.5.1", "Unrecognised command")
}

/// `553` for a `MAIL`/`RCPT` carrying a non-ASCII address.
///
/// RFC 6531 §3.7.4.1 names `553 5.6.7` for a non-ASCII address the server will
/// not accept. Without it the line falls through to `500 Unrecognised command`,
/// which tells the developer their mail library sent nonsense when it sent a
/// perfectly good internationalised address.
#[must_use]
pub fn address_needs_smtputf8() -> Reply {
    Reply::enhanced(
        Code::MailboxNameNotAllowed,
        "5.6.7",
        "Non-ASCII addresses need SMTPUTF8, which Sandesh does not support yet",
    )
}

/// `501` malformed arguments.
#[must_use]
pub fn syntax_error(detail: impl Into<String>) -> Reply {
    Reply::enhanced(Code::ParameterSyntaxError, "5.5.4", detail)
}

/// `502` recognised but not implemented.
#[must_use]
pub fn not_implemented(command: &str) -> Reply {
    Reply::enhanced(
        Code::NotImplemented,
        "5.5.1",
        format!("{command} not implemented"),
    )
}

/// `503` command issued in the wrong state.
#[must_use]
pub fn bad_sequence(detail: impl Into<String>) -> Reply {
    Reply::enhanced(Code::BadSequence, "5.5.1", detail)
}

/// `504` a parameter is recognised but unsupported.
#[must_use]
pub fn parameter_not_implemented(detail: impl Into<String>) -> Reply {
    Reply::enhanced(Code::ParameterNotImplemented, "5.5.4", detail)
}

/// `552` message exceeds the configured size.
#[must_use]
pub fn message_too_large(limit: usize) -> Reply {
    Reply::enhanced(
        Code::MessageTooLarge,
        "5.3.4",
        format!("Message exceeds the {limit} byte limit"),
    )
}

/// `452` too many recipients in one transaction.
#[must_use]
pub fn too_many_recipients(limit: usize) -> Reply {
    Reply::enhanced(
        Code::InsufficientStorage,
        "4.5.3",
        format!("Too many recipients; the limit is {limit}"),
    )
}

/// `550` the recipient does not exist.
#[must_use]
pub fn no_such_user(address: &str) -> Reply {
    Reply::enhanced(
        Code::MailboxUnavailablePermanent,
        "5.1.1",
        format!("No such user: {address}"),
    )
}

/// `451` an internal failure the client may retry.
///
/// Takes no detail argument by design: internal error text must never reach a
/// client, both because it leaks implementation detail and because it is
/// meaningless to the sender. The detail is logged instead.
#[must_use]
pub fn internal_error() -> Reply {
    Reply::enhanced(
        Code::LocalError,
        "4.3.0",
        "Internal error processing the message",
    )
}

/// `252` — cannot verify, but will accept.
///
/// The correct non-disclosing answer to `VRFY`: confirming which addresses
/// exist is an address-harvesting oracle.
#[must_use]
pub fn cannot_verify() -> Reply {
    Reply::enhanced(
        Code::CannotVerify,
        "2.5.2",
        "Cannot verify user; will accept message and attempt delivery",
    )
}

/// `421` the server is shutting down or the session timed out.
#[must_use]
pub fn service_unavailable(reason: &str) -> Reply {
    Reply::enhanced(Code::ServiceUnavailable, "4.3.2", reason)
}

/// `550` a recipient refused by a configured fault (ADR-084).
///
/// Says plainly that the refusal is injected. A developer testing resilience
/// benefits from the reply naming its own cause — and a developer who forgot
/// the fault is configured needs it, which is the failure mode ADR-084 is most
/// concerned with. Nothing here is a real mailbox judgement, so it must not
/// read like one.
#[must_use]
pub fn fault_recipient_rejected() -> Reply {
    Reply::enhanced(
        Code::MailboxUnavailablePermanent,
        "5.7.1",
        "Rejected by an injected fault",
    )
}

/// `554` a message refused by a configured fault (ADR-084).
#[must_use]
pub fn fault_transaction_failed() -> Reply {
    Reply::enhanced(
        Code::TransactionFailed,
        "5.7.1",
        "Transaction failed by an injected fault",
    )
}

/// `451` a temporary refusal from a configured fault (ADR-084).
///
/// A `4xx` because the mode exists to exercise a client's retry path: a client
/// that treats this as permanent and drops the message has the bug the fault
/// was configured to find.
#[must_use]
pub fn fault_temporary_failure() -> Reply {
    Reply::enhanced(
        Code::LocalError,
        "4.7.1",
        "Temporary failure from an injected fault",
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn single_line_reply_is_well_formed() {
        let wire = ok("Done").to_wire();
        assert_eq!(wire, "250 2.0.0 Done\r\n");
    }

    #[test]
    fn multiline_uses_hyphen_until_the_final_line() {
        // The classic EHLO bug: a wrong separator hangs or truncates clients.
        let wire = Reply::multiline(
            Code::Ok,
            vec![
                String::from("sandesh"),
                String::from("SIZE 100"),
                String::from("8BITMIME"),
            ],
        )
        .to_wire();

        assert_eq!(wire, "250-sandesh\r\n250-SIZE 100\r\n250 8BITMIME\r\n");
    }

    #[test]
    fn every_line_ends_with_crlf() {
        for reply in [
            greeting("host"),
            start_mail_input(),
            Reply::multiline(Code::Ok, vec![String::from("a"), String::from("b")]),
        ] {
            let wire = reply.to_wire();
            for line in wire.split_inclusive('\n') {
                assert!(line.ends_with("\r\n"), "line not CRLF-terminated: {line:?}");
            }
        }
    }

    #[test]
    fn enhanced_status_appears_only_on_the_final_line() {
        // RFC 2034 §4.
        let reply = Reply {
            code: Code::Ok,
            enhanced: Some("2.0.0"),
            lines: vec![String::from("first"), String::from("second")],
        };
        let wire = reply.to_wire();
        assert_eq!(wire, "250-first\r\n250 2.0.0 second\r\n");
    }

    #[test]
    fn codes_carry_their_numeric_value() {
        assert_eq!(Code::ServiceReady.value(), 220);
        assert_eq!(Code::StartMailInput.value(), 354);
        assert_eq!(Code::MessageTooLarge.value(), 552);
    }

    #[test]
    fn closing_codes_are_identified() {
        assert!(Code::ServiceClosing.is_closing());
        assert!(Code::ServiceUnavailable.is_closing());
        assert!(!Code::Ok.is_closing());
    }

    #[test]
    fn positive_and_negative_codes_are_distinguished() {
        assert!(Code::Ok.is_positive());
        assert!(Code::StartMailInput.is_positive());
        assert!(!Code::BadSequence.is_positive());
        assert!(!Code::LocalError.is_positive());
    }

    #[test]
    fn internal_errors_never_leak_detail_to_the_client() {
        let wire = internal_error().to_wire();
        assert!(wire.contains("Internal error"));
        assert!(!wire.to_lowercase().contains("sql"));
        assert!(!wire.to_lowercase().contains("panic"));
    }

    #[test]
    fn vrfy_does_not_disclose_whether_an_address_exists() {
        assert_eq!(cannot_verify().code(), Code::CannotVerify);
    }
}
