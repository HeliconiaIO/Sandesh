//! The message entity and its authoritative raw representation.

use crate::{
    AttachmentId, ByteRange, EmailAddress, MessageId, MimePart, ParseDiagnostic, PartPath,
    Timestamp,
};
use sha2::{Digest, Sha256};

/// The exact octets of a message, as received.
///
/// This is the **authoritative** representation (ADR-003). Everything else —
/// headers, MIME parts, attachments — is derived and regenerable. Bytes are
/// stored verbatim: no line-ending normalisation, no re-encoding, no repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawMessage {
    bytes: Vec<u8>,
    sha256: [u8; 32],
}

impl RawMessage {
    /// Takes ownership of the received bytes and computes their digest.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        let sha256 = Sha256::digest(&bytes).into();
        Self { bytes, sha256 }
    }

    /// Reconstructs from storage without recomputing the digest.
    #[must_use]
    pub const fn from_parts(bytes: Vec<u8>, sha256: [u8; 32]) -> Self {
        Self { bytes, sha256 }
    }

    /// The exact octets.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `RFC822.SIZE` — the authoritative octet count.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// The SHA-256 digest, used for integrity checks and dedup.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// Whether the stored digest matches the stored bytes.
    ///
    /// Guards against silent corruption between write and read.
    #[must_use]
    pub fn verify_integrity(&self) -> bool {
        let computed: [u8; 32] = Sha256::digest(&self.bytes).into();
        computed == self.sha256
    }
}

/// One header field, with its exact bytes and position preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Field name with its original casing.
    pub name: String,
    /// Field value bytes exactly as received, folding and encoded-words intact.
    pub raw_value: Vec<u8>,
    /// RFC 2047-decoded value, best effort.
    pub decoded_value: String,
    /// Position of the whole field within the raw message.
    pub range: ByteRange,
}

impl Header {
    /// Whether the name matches, case-insensitively (RFC 5322 §1.2.2).
    #[must_use]
    pub fn name_matches(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// The SMTP envelope, distinct from the message headers.
///
/// `RCPT TO` is what determines delivery, not `To:`/`Cc:` — which is the only
/// way `Bcc` testing can work (`CORE_SPEC.md` §7).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Envelope {
    /// `MAIL FROM`. `None` is the null reverse-path `<>` used by bounces.
    pub mail_from: Option<EmailAddress>,
    /// `RCPT TO` addresses, which may differ entirely from the headers.
    pub rcpt_to: Vec<EmailAddress>,
    /// The `HELO`/`EHLO` domain the client announced.
    pub helo_domain: Option<String>,
    /// The peer address, as text so the core stays free of socket types.
    pub peer: Option<String>,
    /// A description of the TLS parameters, when the session was encrypted.
    pub tls: Option<String>,
    /// The authenticated identity, when the session authenticated.
    pub auth_identity: Option<String>,
}

/// How a message entered the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverySource {
    /// Received over SMTP.
    Smtp,
    /// Uploaded via IMAP `APPEND`.
    ImapAppend,
    /// Injected through the REST API.
    Api,
}

impl DeliverySource {
    /// The persisted discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::ImapAppend => "imap_append",
            Self::Api => "api",
        }
    }

    /// Parses a persisted discriminant.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smtp" => Some(Self::Smtp),
            "imap_append" => Some(Self::ImapAppend),
            "api" => Some(Self::Api),
            _ => None,
        }
    }
}

/// Derived, regenerable structure extracted from the raw bytes.
///
/// Carries `parse_version` so a parser upgrade can invalidate and rebuild this
/// from the untouched raw message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    /// Top-level headers in received order, duplicates preserved.
    pub headers: Vec<Header>,
    /// Every MIME part, flattened; the tree is navigable via [`PartPath`].
    pub parts: Vec<MimePart>,
    /// Paths of `text/plain` body parts.
    pub text_parts: Vec<PartPath>,
    /// Paths of `text/html` body parts.
    pub html_parts: Vec<PartPath>,
    /// Attachment metadata projected from the parts.
    pub attachments: Vec<Attachment>,
    /// Deviations observed while parsing.
    pub diagnostics: Vec<ParseDiagnostic>,
    /// Whether parsing completed without error-level diagnostics.
    pub parse_ok: bool,
    /// Parser version that produced this structure.
    pub parse_version: u32,
}

impl ParsedMessage {
    /// The first header matching `name`, case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&Header> {
        self.headers.iter().find(|h| h.name_matches(name))
    }

    /// Every header matching `name`, in received order.
    pub fn headers_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Header> + 'a {
        self.headers.iter().filter(move |h| h.name_matches(name))
    }

    /// The part at `path`.
    #[must_use]
    pub fn part(&self, path: &PartPath) -> Option<&MimePart> {
        self.parts.iter().find(|p| &p.path == path)
    }

    /// The root part, if the message has any structure.
    #[must_use]
    pub fn root_part(&self) -> Option<&MimePart> {
        self.parts.iter().find(|p| p.path.is_root())
    }
}

/// Attachment metadata — a projection over a [`MimePart`], not a second copy.
///
/// The bytes always remain in the raw message; `size` is the *decoded* length,
/// which is what a client is told and what a download produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Identity of this attachment record.
    pub id: AttachmentId,
    /// The part this attachment projects.
    pub part_path: PartPath,
    /// Filename, when the message supplied one.
    pub filename: Option<String>,
    /// Media type.
    pub content_type: String,
    /// `Content-ID`, for inline resources referenced by `cid:`.
    pub content_id: Option<String>,
    /// Decoded size in bytes.
    pub size: u64,
    /// Whether this is an inline resource rather than a detached attachment.
    pub is_inline: bool,
}

/// Immutable message content.
///
/// A `Message` never changes after creation. Everything a protocol can mutate —
/// flags, UID, mailbox membership — lives on
/// [`MessageEntry`](crate::MessageEntry). That immutability is what makes it
/// safe for one message to appear in many mailboxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Identity.
    pub id: MessageId,
    /// Authoritative bytes.
    pub raw: RawMessage,
    /// Derived structure.
    pub parsed: ParsedMessage,
    /// SMTP-level envelope.
    pub envelope: Envelope,
    /// How the message arrived.
    pub source: DeliverySource,
    /// When the message was received.
    pub received_at: Timestamp,
}

impl Message {
    /// `RFC822.SIZE`.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.raw.size()
    }

    /// The decoded `Subject`, if present.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.parsed
            .header("subject")
            .map(|h| h.decoded_value.as_str())
    }

    /// The decoded `Message-ID`, if present.
    #[must_use]
    pub fn message_id_header(&self) -> Option<&str> {
        self.parsed
            .header("message-id")
            .map(|h| h.decoded_value.as_str())
    }

    /// The bytes of the part at `path`, sliced from the raw message.
    ///
    /// This is the operation behind IMAP `FETCH BODY[<path>]`, and it is exact
    /// because it slices the original octets rather than re-serialising.
    #[must_use]
    pub fn part_body(&self, path: &PartPath) -> Option<&[u8]> {
        self.parsed.part(path)?.body(self.raw.bytes())
    }

    /// The decoded bytes of the part at `path`.
    ///
    /// Unlike [`part_body`](Self::part_body), which returns the raw, possibly
    /// transfer-encoded bytes for protocol operations like IMAP `FETCH BODY[...]`,
    /// this decodes `Content-Transfer-Encoding` (Base64, Quoted-Printable) and charset
    /// for UI preview and attachment export.
    #[must_use]
    pub fn part_body_decoded(&self, path: &PartPath) -> Option<Vec<u8>> {
        crate::parser::decoded_part_body(self.raw.bytes(), path)
            .or_else(|| self.part_body(path).map(<[u8]>::to_vec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_verifies() {
        let raw = RawMessage::new(b"Subject: test\r\n\r\nbody".to_vec());
        assert!(raw.verify_integrity());
        assert_eq!(raw.sha256(), RawMessage::new(raw.bytes().to_vec()).sha256());
    }

    #[test]
    fn size_counts_octets_not_characters() {
        // RFC822.SIZE is octets; a multi-byte character must count as its bytes.
        let raw = RawMessage::new("é".as_bytes().to_vec());
        assert_eq!(raw.size(), 2);
    }

    #[test]
    fn corruption_is_detected() {
        let good = RawMessage::new(b"original".to_vec());
        let tampered = RawMessage::from_parts(b"modified".to_vec(), *good.sha256());
        assert!(!tampered.verify_integrity());
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let h = Header {
            name: String::from("Subject"),
            raw_value: b" hello".to_vec(),
            decoded_value: String::from("hello"),
            range: ByteRange::empty_at(0),
        };
        assert!(h.name_matches("subject"));
        assert!(h.name_matches("SUBJECT"));
        assert!(!h.name_matches("subj"));
    }

    #[test]
    fn delivery_source_round_trips() {
        for source in [
            DeliverySource::Smtp,
            DeliverySource::ImapAppend,
            DeliverySource::Api,
        ] {
            assert_eq!(DeliverySource::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn null_reverse_path_envelope_is_representable() {
        let envelope = Envelope {
            mail_from: None,
            rcpt_to: vec![EmailAddress::parse_lenient("dev@example.test")],
            ..Envelope::default()
        };
        assert!(envelope.mail_from.is_none(), "bounces use <>");
        assert_eq!(envelope.rcpt_to.len(), 1);
    }
}
