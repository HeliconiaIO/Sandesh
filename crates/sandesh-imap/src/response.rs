//! IMAP response construction (RFC 9051 §7).
//!
//! # The three response forms
//!
//! ```text
//! * OK [CAPABILITY ...] Sandesh ready     ← untagged: data, or the greeting
//! + ready for literal data                ← continuation request
//! A001 OK LOGIN completed                 ← tagged: this command is finished
//! ```
//!
//! Exactly one tagged response ends each command, and it echoes the client's
//! tag. Getting that wrong desynchronises every client, because a client
//! matches responses to commands by tag alone.
//!
//! # Why a builder rather than `format!` at each call site
//!
//! Two reasons, both about safety rather than tidiness. First, every response
//! must end with CRLF and nothing else may contain a bare CR or LF — a newline
//! smuggled into a response is a forged response line to the client. Second,
//! the capability list must be derived from what the server can actually do, in
//! one place, or it drifts from the implementation (phase brief §5).

use crate::command::Tag;
use sandesh_config::AuthMode;
use sandesh_proto::Mechanism;

/// The completion status of a command (RFC 9051 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The command succeeded.
    Ok,
    /// The command was understood but refused — wrong state, bad credentials.
    No,
    /// The command was not understood, or was a protocol error.
    Bad,
}

impl Status {
    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::No => "NO",
            Self::Bad => "BAD",
        }
    }
}

/// What the server can currently do, as advertised by `CAPABILITY`.
///
/// Built from configuration and the connection's encryption state, so the list
/// cannot claim something the server will then refuse. The struct exists rather
/// than a free function because the same value answers both the `CAPABILITY`
/// command and the capability code in the greeting, and they must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    auth_mode: AuthMode,
    /// Whether `STARTTLS` can actually be honoured right now.
    start_tls_available: bool,
    /// Whether this connection is already encrypted.
    encrypted: bool,
    /// Whether an event source was injected, making `IDLE` answerable.
    idle_available: bool,
}

impl Capabilities {
    /// Describes what a connection may do.
    ///
    /// `start_tls_available` must be false on an already-encrypted connection;
    /// [`Self::new`] enforces that rather than trusting the caller, because
    /// advertising `STARTTLS` inside TLS invites a client to renegotiate
    /// (RFC 9051 §6.2.1 forbids it).
    #[must_use]
    pub const fn new(auth_mode: AuthMode, start_tls_configured: bool, encrypted: bool) -> Self {
        Self {
            auth_mode,
            start_tls_available: start_tls_configured && !encrypted,
            encrypted,
            idle_available: false,
        }
    }

    /// Records whether this session can actually honour `IDLE`.
    ///
    /// `IDLE` is advertised only when an event source was injected. A session
    /// without one can accept the command but has nothing to report, and
    /// advertising it would tell a client to wait for updates that cannot
    /// arrive — the "capability this server cannot honour" failure the list
    /// exists to prevent.
    #[must_use]
    pub const fn with_idle(mut self, available: bool) -> Self {
        self.idle_available = available;
        self
    }

    /// Whether `STARTTLS` is advertised, and therefore honoured.
    #[must_use]
    pub const fn advertises_start_tls(self) -> bool {
        self.start_tls_available
    }

    /// Whether a SASL mechanism may be used on this connection.
    ///
    /// Delegates to [`AuthMode::is_offered_over`], the same policy SMTP uses:
    /// `PLAIN` and `LOGIN` both send the password, so neither is offered in the
    /// clear (ADR-033). One policy function, two protocols — a second copy here
    /// is exactly how the two would come to disagree.
    #[must_use]
    pub const fn offers_auth(self) -> bool {
        self.auth_mode.is_offered_over(self.encrypted)
    }

    /// The capability list, in the order it is sent.
    ///
    /// `IMAP4rev2` comes first as RFC 9051 §7.2.2 requires. Everything after it
    /// is present only if it works:
    ///
    /// - `STARTTLS` only when a TLS acceptor exists and the connection is not
    ///   already encrypted.
    /// - `LOGINDISABLED` whenever `AUTH` is unavailable on this connection,
    ///   which tells the client not to try `LOGIN` rather than letting it fail
    ///   (RFC 9051 §7.2.2).
    /// - `AUTH=` only for mechanisms that are genuinely usable now.
    ///
    /// `IDLE` (RFC 2177) appears only when an event source was injected, and
    /// `MOVE` (RFC 6851), `UIDPLUS` (RFC 4315), `UNSELECT` (RFC 3691),
    /// `SASL-IR` (RFC 4959) and `LITERAL-` (RFC 7888) unconditionally, because
    /// they are implemented for every session. A capability this server cannot
    /// honour is worse than a missing one, because a client that sees it stops
    /// using the fallback that works — which is also why the bounded
    /// `LITERAL-` is advertised in place of `LITERAL+`.
    #[must_use]
    pub fn to_list(self) -> Vec<String> {
        let mut caps = vec![String::from("IMAP4rev2"), String::from("IMAP4rev1")];

        // Implemented for every session, so unconditional. `UIDPLUS` covers the
        // `COPYUID` and `APPENDUID` codes that `COPY`, `MOVE` and `APPEND` send.
        caps.push(String::from("MOVE"));

        // `NAMESPACE` (RFC 2342) and `ENABLE` (RFC 5161) are likewise
        // unconditional: both are answered from the mailbox model and the
        // session itself, with no configuration or injected dependency that
        // could make either unanswerable.
        caps.push(String::from("NAMESPACE"));
        caps.push(String::from("ENABLE"));

        // `CONDSTORE` (RFC 7162 §3) — advertised from the same predicate that
        // decides whether `ENABLE CONDSTORE` succeeds, so the capability list
        // and `* ENABLED` cannot drift apart. Note what is still *not* here:
        // `QRESYNC` is recognised by `ENABLE` and refused by it, because it
        // needs a durable record of expunged UIDs this server does not keep.
        if crate::enable::Extension::CondStore.is_implemented() {
            caps.push(String::from("CONDSTORE"));
        }

        // `ESEARCH` (RFC 9051 §6.4.4), `LIST-EXTENDED` (RFC 5258) and
        // `SPECIAL-USE` (RFC 6154) are likewise implemented for every session.
        // `SPECIAL-USE` covers both halves RFC 6154 defines: the attributes in
        // `LIST` output, and `CREATE ... (USE (…))`.
        caps.push(String::from("ESEARCH"));
        caps.push(String::from("LIST-EXTENDED"));
        caps.push(String::from("SPECIAL-USE"));
        caps.push(String::from("UIDPLUS"));

        // `UNSELECT` (RFC 3691) — `CommandKind::Unselect` is dispatched
        // alongside `CLOSE`, differing only in that it never expunges.
        caps.push(String::from("UNSELECT"));

        // `SASL-IR` (RFC 4959) — `AUTHENTICATE` already reads an optional
        // initial response from the second argument, so a client that sees this
        // saves a round trip on every login. Advertised unconditionally rather
        // than behind `offers_auth()`: RFC 4959 §3 makes it a property of the
        // `AUTHENTICATE` grammar the parser accepts, not of any one mechanism.
        caps.push(String::from("SASL-IR"));

        // `LITERAL-`, deliberately not `LITERAL+` (RFC 7888). The parser
        // accepts `{n+}` and the session suppresses the continuation for it,
        // but `max_literal_size` still bounds `n` and an over-large literal is
        // refused. That is exactly the distinction RFC 7888 §4 draws: `LITERAL+`
        // promises to accept a non-synchronising literal of *any* size, which
        // this server does not do, while `LITERAL-` is the bounded form. The
        // stricter name is the honest one — a client that saw `LITERAL+` here
        // would be promised something the size cap can refuse.
        caps.push(String::from("LITERAL-"));

        if self.idle_available {
            caps.push(String::from("IDLE"));
        }

        if self.start_tls_available {
            caps.push(String::from("STARTTLS"));
        }

        if self.offers_auth() {
            for mechanism in Mechanism::advertised() {
                caps.push(format!("AUTH={}", mechanism.as_str()));
            }
        } else if self.auth_mode != AuthMode::Disabled {
            // No usable AUTH when auth is enabled: say so explicitly. RFC 9051 §7.2.2 defines
            // LOGINDISABLED for exactly this, and a client that sees it will
            // offer to enable TLS instead of prompting for a doomed password.
            // When AuthMode::Disabled, authentication is not enforced (developer mode),
            // so LOGINDISABLED is suppressed to allow cleartext dev testing (ADR-042).
            caps.push(String::from("LOGINDISABLED"));
        }

        caps
    }

    /// The capability list as a space-separated string.
    #[must_use]
    pub fn to_line(self) -> String {
        self.to_list().join(" ")
    }
}

/// The greeting sent on connection (RFC 9051 §7.1.1).
///
/// Untagged `OK` carrying the capability list, so a client needs no round trip
/// to discover what the server offers. `PREAUTH` is deliberately never used:
/// it would place the connection directly in the Authenticated state, and
/// nothing about a TCP connection to this server justifies that.
#[must_use]
pub fn greeting(capabilities: Capabilities, hostname: &str) -> String {
    format!(
        "* OK [CAPABILITY {}] {} Sandesh IMAP4rev2 ready\r\n",
        capabilities.to_line(),
        sanitise(hostname)
    )
}

/// The untagged `CAPABILITY` data line (RFC 9051 §7.2.2).
#[must_use]
pub fn capability_data(capabilities: Capabilities) -> String {
    format!("* CAPABILITY {}\r\n", capabilities.to_line())
}

/// A tagged completion response.
#[must_use]
pub fn tagged(tag: &Tag, status: Status, text: &str) -> String {
    format!("{tag} {} {}\r\n", status.as_str(), sanitise(text))
}

/// A tagged response carrying a response code, e.g. `[CAPABILITY ...]`.
#[must_use]
pub fn tagged_with_code(tag: &Tag, status: Status, code: &str, text: &str) -> String {
    format!(
        "{tag} {} [{}] {}\r\n",
        status.as_str(),
        sanitise(code),
        sanitise(text)
    )
}

/// An untagged `BYE`, which precedes every connection close (RFC 9051 §7.1.5).
#[must_use]
pub fn bye(text: &str) -> String {
    format!("* BYE {}\r\n", sanitise(text))
}

/// A continuation request, inviting the client to send more (RFC 9051 §7.5).
///
/// `data` is the base64 challenge for a SASL exchange, or empty for a literal.
/// A bare `+` with no text is valid and is what a literal continuation uses.
#[must_use]
pub fn continuation(data: &str) -> String {
    if data.is_empty() {
        String::from("+ \r\n")
    } else {
        format!("+ {}\r\n", sanitise(data))
    }
}

/// A `BAD` response for input with no usable tag (RFC 9051 §7.1.3).
///
/// When the tag itself is unparseable there is nothing to echo, so `*` stands
/// in: RFC 9051 §2.2.1 allows it, and inventing a tag would be worse — the
/// client would match the response to a command it never sent.
#[must_use]
pub fn untagged_bad(text: &str) -> String {
    format!("* BAD {}\r\n", sanitise(text))
}

/// One untagged `LIST` response line (RFC 9051 §7.3.1).
///
/// ```text
/// * LIST (\Sent) "/" "Sent Items"
///   ▲    ▲        ▲   ▲
///   │    │        │   └─ the mailbox name, quoted when it needs to be
///   │    │        └───── the hierarchy delimiter, always quoted
///   │    └────────────── name attributes, space separated, possibly empty
///   └─────────────────── the untagged marker
/// ```
///
/// `attributes` are emitted in the order given; the caller sorts them so the
/// output is deterministic.
#[must_use]
pub fn list_line(attributes: &[String], delimiter: char, name: &str) -> String {
    format!(
        "* LIST ({}) \"{delimiter}\" {}\r\n",
        attributes.join(" "),
        crate::mailbox::quote_mailbox(name)
    )
}

/// One untagged `STATUS` response line (RFC 9051 §7.3.3).
///
/// ```text
/// * STATUS "Work" (MESSAGES 12 UIDNEXT 47 UIDVALIDITY 3 UNSEEN 2)
/// ```
///
/// `items` are already-rendered `NAME value` pairs, in the order the client
/// asked for them — RFC 9051 §6.3.11 does not require a particular order, and
/// echoing the request order is what clients find least surprising.
#[must_use]
pub fn status_line(name: &str, items: &[String]) -> String {
    format!(
        "* STATUS {} ({})\r\n",
        crate::mailbox::quote_mailbox(name),
        items.join(" ")
    )
}

/// The untagged `ESEARCH` response (RFC 9051 §7.3.4).
///
/// ```text
/// * ESEARCH (TAG "A1") UID MIN 2 MAX 9 ALL 2,5:7,9 COUNT 5
///            ▲          ▲   ▲
///            │          │   └─ the requested return data, in a fixed order
///            │          └───── present only for UID SEARCH
///            └──────────────── correlates the result with the command
/// ```
///
/// `items` are already-rendered `NAME value` pairs, built by the caller in
/// [`crate::search::ReturnOption::ALL_OPTIONS`] order.
///
/// # Why the tag is echoed
///
/// The correlator is what makes `ESEARCH` safe where `* SEARCH` is not. An
/// untagged `* SEARCH` line carries nothing tying it to a command, so a client
/// receiving one while another command is in flight cannot tell which it
/// answers. RFC 9051 §7.3.4 fixes that by naming the tag.
///
/// # `UID` is a claim about the numbers
///
/// The `UID` marker tells the client that every number in this response is a
/// UID rather than a sequence number. Emitting it when the numbers are
/// sequence numbers — or omitting it when they are UIDs — is worse than a
/// malformed response: the client parses it happily and addresses the wrong
/// messages. The caller passes `uid_mode` straight from the command, the same
/// flag that chose the numbers.
#[must_use]
pub fn esearch_line(tag: &Tag, uid_mode: bool, items: &[String]) -> String {
    let mut out = format!("* ESEARCH (TAG \"{}\")", sanitise(tag.as_str()));
    if uid_mode {
        out.push_str(" UID");
    }
    for item in items {
        out.push(' ');
        out.push_str(item);
    }
    out.push_str("\r\n");
    out
}

/// The untagged `NAMESPACE` response (RFC 2342 §5).
///
/// ```text
/// * NAMESPACE (("" "/")) NIL NIL
///              ▲   ▲     ▲   ▲
///              │   │     │   └─ other users' namespaces: none exist
///              │   │     └───── shared namespaces: none exist
///              │   └─────────── the hierarchy delimiter
///              └─────────────── the personal namespace prefix: empty
/// ```
///
/// The shape is dictated by the mailbox model, not by choice.
/// [`sandesh_core::MailboxStore::list`] takes an `AccountId` and returns that
/// account's mailboxes and nothing else, so every name a client can reach is
/// personal and reachable with no prefix at all. That is exactly one personal
/// namespace with an empty prefix.
///
/// Shared and other-users' namespaces are `NIL` for the same reason: there is
/// no storage concept behind either one. RFC 2342 §5 distinguishes `NIL` ("this
/// server has none") from an empty list ("it has some, but you may see none"),
/// and `NIL` is the truthful answer here. Returning a fabricated `("#shared"
/// "/")` would advertise a hierarchy that no `LIST` could ever enumerate.
///
/// The delimiter is [`sandesh_core::HIERARCHY_DELIMITER`], the same constant
/// `LIST` reports, so the two responses cannot drift apart.
#[must_use]
pub fn namespace_line(delimiter: char) -> String {
    format!("* NAMESPACE ((\"\" \"{delimiter}\")) NIL NIL\r\n")
}

/// The untagged `ENABLED` response (RFC 5161 §3.2).
///
/// ```text
/// * ENABLED CONDSTORE      ← the server took these on
/// * ENABLED                ← nothing was enabled; the line is still sent
/// ```
///
/// Only capabilities the server actually enabled are listed. RFC 5161 §3.1 is
/// explicit that an unrecognised capability is ignored rather than an error, so
/// the omission *is* the refusal — which is why `ENABLE` never fails and this
/// line is sent even when it is empty.
#[must_use]
pub fn enabled_line(capabilities: &[String]) -> String {
    let mut out = String::from("* ENABLED");
    for capability in capabilities {
        out.push(' ');
        out.push_str(&sanitise(capability));
    }
    out.push_str("\r\n");
    out
}

/// An untagged data line carrying a count, e.g. `* 12 EXISTS`.
#[must_use]
pub fn count_line(count: usize, keyword: &str) -> String {
    format!("* {count} {}\r\n", sanitise(keyword))
}

/// An untagged `OK` carrying a response code, e.g. `* OK [UIDNEXT 47] …`.
#[must_use]
pub fn untagged_ok_with_code(code: &str, text: &str) -> String {
    format!("* OK [{}] {}\r\n", sanitise(code), sanitise(text))
}

/// The untagged `SEARCH` result line (RFC 9051 §7.3.4).
///
/// ```text
/// * SEARCH 2 5 9
/// * SEARCH                ← no matches; the line is still sent
/// ```
///
/// The numbers are sequence numbers for `SEARCH` and UIDs for `UID SEARCH`;
/// this function does not know or care which, because the caller has already
/// resolved them — mixing the two up is a caller bug that no formatting rule
/// could catch.
///
/// An empty result still emits the bare `* SEARCH` line. RFC 9051 §7.3.4
/// requires it, and clients that expect the line will hang or mis-parse
/// without it, so "no matches" must not be silence.
#[must_use]
pub fn search_line(numbers: &[u32]) -> String {
    if numbers.is_empty() {
        return String::from("* SEARCH\r\n");
    }
    let rendered: Vec<String> = numbers.iter().map(u32::to_string).collect();
    format!("* SEARCH {}\r\n", rendered.join(" "))
}

/// Strips CR and LF from response text.
///
/// The security property behind every `format!` above: a response is one line
/// terminated by exactly one CRLF. Text reaching these functions can be
/// attacker-influenced — a mechanism name, a mailbox name — and an embedded
/// newline would let a client inject a whole forged response line into the
/// stream it is reading. Replacing rather than rejecting keeps the caller
/// infallible; there is no legitimate newline in any of this text.
fn sanitise(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// The capabilities of a cleartext connection with STARTTLS available.
    fn cleartext_with_start_tls() -> Capabilities {
        Capabilities::new(AuthMode::Required, true, false)
    }

    #[test]
    fn imap4rev2_comes_first() {
        // RFC 9051 §7.2.2 requires it as the first element.
        let caps = cleartext_with_start_tls().to_list();
        assert_eq!(caps.first().map(String::as_str), Some("IMAP4rev2"));
    }

    #[test]
    fn idle_is_not_advertised_without_an_event_source() {
        // The truthfulness rule, in its sharpest form: a session with no event
        // source can never report a change, so telling a client to wait for one
        // would strand it. `with_idle` defaults to false precisely so that a
        // caller which forgets to set it under-promises rather than over-promises.
        for encrypted in [false, true] {
            for auth in [AuthMode::Disabled, AuthMode::Optional, AuthMode::Required] {
                let line = Capabilities::new(auth, true, encrypted).to_line();
                assert!(!line.contains("IDLE"), "IDLE advertised in: {line}");

                let disabled = Capabilities::new(auth, true, encrypted)
                    .with_idle(false)
                    .to_line();
                assert!(!disabled.contains("IDLE"), "IDLE advertised in: {disabled}");
            }
        }
    }

    #[test]
    fn idle_is_advertised_once_an_event_source_exists() {
        for encrypted in [false, true] {
            let line = Capabilities::new(AuthMode::Required, true, encrypted)
                .with_idle(true)
                .to_line();
            assert!(line.contains("IDLE"), "IDLE missing from: {line}");
        }
    }

    #[test]
    fn namespace_reports_one_personal_namespace_and_no_others() {
        // The exact wire form of RFC 2342 §5 for this mailbox model. Every
        // token here is load-bearing, which is why the whole line is asserted
        // rather than a substring: a wrong delimiter, a non-empty prefix or a
        // fabricated shared namespace each break mailbox discovery in a way a
        // looser assertion would miss.
        assert_eq!(
            namespace_line('/'),
            "* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n"
        );
    }

    #[test]
    fn namespace_uses_the_delimiter_it_is_given() {
        // The delimiter is not hard-coded in the formatter; the caller passes
        // `HIERARCHY_DELIMITER`, the same constant `LIST` reports. A mutation
        // that hard-coded '/' here would survive the test above and be caught
        // by this one.
        assert_eq!(
            namespace_line('.'),
            "* NAMESPACE ((\"\" \".\")) NIL NIL\r\n"
        );
    }

    #[test]
    fn namespace_matches_the_delimiter_list_reports() {
        // The two responses must agree: a client that learns '/' from
        // NAMESPACE and then cannot parse a LIST line is broken by the
        // disagreement, not by either line alone.
        let delimiter = sandesh_core::HIERARCHY_DELIMITER;
        let listed = list_line(&[], delimiter, "Sent");
        assert!(namespace_line(delimiter).contains(&format!("\"{delimiter}\"")));
        assert!(listed.contains(&format!("\"{delimiter}\"")));
    }

    #[test]
    fn enabled_line_lists_what_was_enabled() {
        assert_eq!(
            enabled_line(&[String::from("CONDSTORE")]),
            "* ENABLED CONDSTORE\r\n"
        );
        assert_eq!(
            enabled_line(&[String::from("CONDSTORE"), String::from("QRESYNC")]),
            "* ENABLED CONDSTORE QRESYNC\r\n"
        );
    }

    #[test]
    fn enabled_line_is_sent_even_when_empty() {
        // RFC 5161 §3.2. The empty line is how a client learns that *none* of
        // what it asked for is available; omitting it would leave the client
        // unable to distinguish that from a lost response.
        assert_eq!(enabled_line(&[]), "* ENABLED\r\n");
    }

    #[test]
    fn implemented_extensions_are_advertised() {
        // The other half of the truthfulness rule. `MOVE` and `UIDPLUS` are
        // implemented for every session, so withholding them would make clients
        // fall back to COPY+STORE+EXPUNGE for a MOVE this server performs
        // atomically per message.
        let line = cleartext_with_start_tls().to_line();
        for present in [
            "MOVE",
            "UIDPLUS",
            "NAMESPACE",
            "ENABLE",
            "ESEARCH",
            "LIST-EXTENDED",
            "SPECIAL-USE",
        ] {
            assert!(line.contains(present), "{present} missing from: {line}");
        }
    }

    #[test]
    fn no_unimplemented_extension_is_advertised() {
        // The truthfulness rule. None of these is implemented, so none may
        // appear. `IDLE`, `MOVE` and `UIDPLUS` have left this list because they
        // are now implemented — and are asserted *present* above rather than
        // simply dropped, so the list cannot quietly become a no-op.
        for encrypted in [false, true] {
            let line = Capabilities::new(AuthMode::Required, true, encrypted)
                .with_idle(true)
                .to_line();
            for absent in [
                "QRESYNC", "SORT", "THREAD", "BINARY", "CATENATE", "COMPRESS",
            ] {
                assert!(!line.contains(absent), "{absent} advertised in: {line}");
            }

            // `CONDSTORE` has left the list above because it is now
            // implemented, and is asserted *present* here — by the same
            // convention `IDLE`, `MOVE` and `UIDPLUS` follow — so the list
            // cannot quietly become a no-op.
            //
            // `QRESYNC` stays absent, and that pairing is the point: CONDSTORE
            // is a prerequisite of QRESYNC, so advertising the first must not
            // drag in the second.
            assert!(
                line.contains("CONDSTORE"),
                "CONDSTORE is implemented and must be advertised: {line}"
            );
        }
    }

    #[test]
    fn auth_is_not_offered_in_the_clear() {
        // ADR-033: PLAIN and LOGIN both send the password.
        let line = Capabilities::new(AuthMode::Required, true, false).to_line();
        assert!(
            !line.contains("AUTH="),
            "credentials offered in clear: {line}"
        );
        assert!(
            line.contains("LOGINDISABLED"),
            "a client must be told not to try LOGIN: {line}"
        );
    }

    #[test]
    fn auth_is_offered_once_encrypted() {
        let line = Capabilities::new(AuthMode::Required, false, true).to_line();
        assert!(line.contains("AUTH=PLAIN"), "{line}");
        assert!(line.contains("AUTH=LOGIN"), "{line}");
        assert!(
            !line.contains("LOGINDISABLED"),
            "AUTH is available, so LOGINDISABLED would be a lie: {line}"
        );
    }

    #[test]
    fn auth_is_not_offered_when_disabled_and_no_logindisabled() {
        let line = Capabilities::new(AuthMode::Disabled, false, true).to_line();
        assert!(!line.contains("AUTH="), "{line}");
        assert!(!line.contains("LOGINDISABLED"), "{line}");
    }

    #[test]
    fn unconditional_capabilities_are_all_advertised() {
        // Each of these is implemented for every session, with no injected
        // dependency that could make it unanswerable. The list drifted once
        // already: `UNSELECT`, `SASL-IR` and `LITERAL-` were all implemented
        // and none were advertised, so clients used the slower fallback.
        let line = Capabilities::new(AuthMode::Disabled, false, false).to_line();
        for capability in [
            "IMAP4rev2",
            "MOVE",
            "NAMESPACE",
            "ENABLE",
            "ESEARCH",
            "LIST-EXTENDED",
            "SPECIAL-USE",
            "UIDPLUS",
            "UNSELECT",
            "SASL-IR",
            "LITERAL-",
        ] {
            assert!(
                line.split(' ').any(|c| c == capability),
                "{capability} is implemented but missing from: {line}"
            );
        }
    }

    #[test]
    fn literal_plus_is_never_advertised() {
        // RFC 7888 §4: `LITERAL+` promises to accept a non-synchronising
        // literal of any size. `max_literal_size` bounds ours, so the bounded
        // `LITERAL-` is the only honest name. A bare `contains` would pass on
        // the `LITERAL-` substring, hence the exact token comparison.
        let line = Capabilities::new(AuthMode::Required, false, true).to_line();
        assert!(
            !line.split(' ').any(|c| c == "LITERAL+"),
            "the size cap can refuse what LITERAL+ promises: {line}"
        );
    }

    #[test]
    fn start_tls_is_not_advertised_inside_tls() {
        // Renegotiating inside an established channel is what RFC 9051 §6.2.1
        // forbids; `Capabilities::new` suppresses it regardless of the flag.
        let caps = Capabilities::new(AuthMode::Required, true, true);
        assert!(!caps.advertises_start_tls());
        assert!(!caps.to_line().contains("STARTTLS"));
    }

    #[test]
    fn start_tls_is_not_advertised_when_unconfigured() {
        let caps = Capabilities::new(AuthMode::Required, false, false);
        assert!(!caps.advertises_start_tls());
        assert!(!caps.to_line().contains("STARTTLS"));
    }

    #[test]
    fn the_greeting_is_untagged_ok_with_capabilities() {
        let greeting = greeting(cleartext_with_start_tls(), "mail.example");
        assert!(
            greeting.starts_with("* OK [CAPABILITY IMAP4rev2"),
            "{greeting}"
        );
        assert!(greeting.ends_with("\r\n"));
        assert!(
            !greeting.contains("PREAUTH"),
            "a TCP connection never justifies PREAUTH"
        );
    }

    #[test]
    fn responses_end_with_exactly_one_crlf() {
        let tag = Tag::parse("A001").expect("valid tag");
        for line in [
            greeting(cleartext_with_start_tls(), "h"),
            capability_data(cleartext_with_start_tls()),
            tagged(&tag, Status::Ok, "done"),
            tagged_with_code(&tag, Status::Ok, "CAPABILITY IMAP4rev2", "done"),
            bye("closing"),
            continuation(""),
            continuation("challenge"),
            untagged_bad("bad"),
        ] {
            assert!(line.ends_with("\r\n"), "missing CRLF: {line:?}");
            assert_eq!(
                line.matches("\r\n").count(),
                1,
                "a response must be exactly one line: {line:?}"
            );
        }
    }

    #[test]
    fn injected_newlines_cannot_forge_a_response_line() {
        // The response-splitting guard: a client-influenced string containing
        // CRLF must not become two response lines.
        let tag = Tag::parse("A001").expect("valid tag");
        let hostile = "ok\r\n* BYE forged";
        let line = tagged(&tag, Status::Ok, hostile);
        assert_eq!(line.matches("\r\n").count(), 1, "{line:?}");
        assert!(
            !line.contains("\r\n* BYE"),
            "forged line survived: {line:?}"
        );
    }

    #[test]
    fn a_hostile_hostname_cannot_split_the_greeting() {
        let line = greeting(cleartext_with_start_tls(), "h\r\n* OK forged");
        assert_eq!(line.matches("\r\n").count(), 1, "{line:?}");
    }

    #[test]
    fn a_bare_continuation_is_plus_space_crlf() {
        // RFC 9051 §7.5. A literal continuation carries no data, and clients
        // are strict about the shape of this line.
        assert_eq!(continuation(""), "+ \r\n");
    }

    #[test]
    fn a_search_result_is_space_separated() {
        assert_eq!(search_line(&[2, 5, 9]), "* SEARCH 2 5 9\r\n");
        assert_eq!(search_line(&[7]), "* SEARCH 7\r\n");
    }

    #[test]
    fn an_empty_search_result_still_sends_the_line() {
        // RFC 9051 §7.3.4: "no matches" is a bare `* SEARCH`, not silence.
        assert_eq!(search_line(&[]), "* SEARCH\r\n");
    }

    #[test]
    fn a_search_line_is_exactly_one_line() {
        for numbers in [
            vec![],
            vec![1],
            vec![1, 2, 3],
            (1..=100).collect::<Vec<u32>>(),
        ] {
            let line = search_line(&numbers);
            assert!(line.ends_with("\r\n"), "{line:?}");
            assert_eq!(line.matches("\r\n").count(), 1, "{line:?}");
        }
    }

    #[test]
    fn tagged_responses_echo_the_tag() {
        let tag = Tag::parse("xyz9").expect("valid tag");
        assert!(tagged(&tag, Status::No, "refused").starts_with("xyz9 NO "));
        assert!(tagged(&tag, Status::Bad, "huh").starts_with("xyz9 BAD "));
    }
}
