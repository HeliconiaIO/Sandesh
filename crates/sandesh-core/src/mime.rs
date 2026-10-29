//! MIME structure as **byte ranges into the authoritative raw message**.
//!
//! No part body is ever copied (`CORE_SPEC.md` §4.3, ADR-003). A part is a
//! `(start, end)` pair into the original octets, which means:
//!
//! * `FETCH BODY[2.1]` is a slice, so it is byte-exact by construction;
//! * memory stays flat regardless of message size;
//! * the UI can highlight a part's exact bytes in the raw source view.

use core::fmt;
use core::ops::Range;

/// A half-open byte range `[start, end)` into a raw message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRange {
    start: usize,
    end: usize,
}

impl ByteRange {
    /// Creates a range, returning `None` if `end < start`.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// An empty range at `position`.
    #[must_use]
    pub const fn empty_at(position: usize) -> Self {
        Self {
            start: position,
            end: position,
        }
    }

    /// The inclusive start offset.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// The exclusive end offset.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// The length in bytes.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Whether the range covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether the range lies entirely within a buffer of `total` bytes.
    #[must_use]
    pub const fn is_within(self, total: usize) -> bool {
        self.end <= total
    }

    /// Slices `raw`, or returns `None` if the range is out of bounds.
    ///
    /// Bounds are re-checked at slice time rather than trusted: ranges may have
    /// been reconstructed from storage, where a bug or corruption could put them
    /// out of step with the bytes.
    #[must_use]
    pub fn slice(self, raw: &[u8]) -> Option<&[u8]> {
        raw.get(self.start..self.end)
    }
}

impl From<ByteRange> for Range<usize> {
    fn from(value: ByteRange) -> Self {
        value.start..value.end
    }
}

impl fmt::Display for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}..{})", self.start, self.end)
    }
}

/// An IMAP part path, such as `1`, `2.1`, `2.1.3` (RFC 9051 §6.4.5).
///
/// Storing the path in IMAP's own numbering makes `FETCH BODY[2.1]` a direct
/// lookup instead of a tree walk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PartPath(Vec<u32>);

impl PartPath {
    /// The root part, whose path renders as an empty string.
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Builds from 1-based component numbers.
    #[must_use]
    pub const fn from_components(components: Vec<u32>) -> Self {
        Self(components)
    }

    /// This path extended by one child index (1-based).
    #[must_use]
    pub fn child(&self, index: u32) -> Self {
        let mut components = self.0.clone();
        components.push(index);
        Self(components)
    }

    /// The parent path, or `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        let mut components = self.0.clone();
        components.pop();
        Some(Self(components))
    }

    /// The path components.
    #[must_use]
    pub fn components(&self) -> &[u32] {
        &self.0
    }

    /// Whether this is the root part.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses the dotted form; the empty string is the root.
    ///
    /// Returns `None` for a component that is not a positive integer.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.is_empty() {
            return Some(Self::root());
        }
        value
            .split('.')
            .map(|c| c.parse::<u32>().ok().filter(|n| *n > 0))
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }
}

impl fmt::Display for PartPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self.0.iter().map(u32::to_string).collect();
        f.write_str(&rendered.join("."))
    }
}

/// A MIME content-transfer-encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferEncoding {
    /// `7bit` — the MIME default when no header is present.
    #[default]
    SevenBit,
    /// `8bit`.
    EightBit,
    /// `binary`.
    Binary,
    /// `quoted-printable`.
    QuotedPrintable,
    /// `base64`.
    Base64,
    /// An encoding we do not recognise; the raw token is preserved elsewhere.
    Other,
}

impl TransferEncoding {
    /// Parses a `Content-Transfer-Encoding` token, case-insensitively.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "7bit" => Self::SevenBit,
            "8bit" => Self::EightBit,
            "binary" => Self::Binary,
            "quoted-printable" => Self::QuotedPrintable,
            "base64" => Self::Base64,
            _ => Self::Other,
        }
    }

    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SevenBit => "7bit",
            Self::EightBit => "8bit",
            Self::Binary => "binary",
            Self::QuotedPrintable => "quoted-printable",
            Self::Base64 => "base64",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for TransferEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a part is intended to be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Disposition {
    /// `inline`, or absent.
    #[default]
    Inline,
    /// `attachment`.
    Attachment,
}

/// One node of the MIME tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MimePart {
    /// IMAP part path.
    pub path: PartPath,
    /// Full media type, lowercased, e.g. `text/plain`.
    pub content_type: String,
    /// `charset` parameter, if present.
    pub charset: Option<String>,
    /// Content-transfer-encoding.
    pub encoding: TransferEncoding,
    /// Content-disposition.
    pub disposition: Disposition,
    /// `filename` from the disposition or `name` from the content type.
    pub filename: Option<String>,
    /// `Content-ID`, with angle brackets stripped.
    pub content_id: Option<String>,
    /// Byte range of this part's own MIME headers.
    pub header_range: ByteRange,
    /// Byte range of this part's body, still encoded.
    pub body_range: ByteRange,
    /// Decoded body length, when known.
    pub decoded_size: Option<u64>,
    /// Whether this part is presented as an attachment.
    pub is_attachment: bool,
    /// Child part paths, in order.
    pub children: Vec<PartPath>,
}

impl MimePart {
    /// Whether body search should look inside this part.
    ///
    /// Text media types only, plus the structured-text `application` types
    /// developers attach (JSON, XML, CSV, calendar invites). A PDF, image or zip
    /// decoded through `from_utf8_lossy` is noise: no search ever matches it,
    /// and indexing it as trigrams made the search index four times the size of
    /// the mail it indexed. Both the index and the live matcher use this one
    /// rule, so they can never disagree about which parts are searched.
    #[must_use]
    pub fn is_searchable_text(&self) -> bool {
        if self.is_multipart() {
            return false;
        }
        let media = self.content_type.as_str();
        media.starts_with("text/")
            || media.starts_with("message/")
            || media.ends_with("+json")
            || media.ends_with("+xml")
            || matches!(
                media,
                "application/json"
                    | "application/xml"
                    | "application/csv"
                    | "application/ics"
                    | "application/javascript"
            )
    }

    /// The part's encoded body bytes, sliced from `raw`.
    #[must_use]
    pub fn body<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        self.body_range.slice(raw)
    }

    /// The part's own header bytes, sliced from `raw`.
    #[must_use]
    pub fn headers<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        self.header_range.slice(raw)
    }

    /// Whether the media type is `multipart/*`.
    #[must_use]
    pub fn is_multipart(&self) -> bool {
        self.content_type.starts_with("multipart/")
    }
}

/// The severity of a parse observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    /// Worth surfacing, but the interpretation is unambiguous.
    Info,
    /// The message deviates from spec and was interpreted heuristically.
    Warning,
    /// The message is malformed; the interpretation may be wrong.
    Error,
}

/// A recorded deviation from the MIME specifications.
///
/// Diagnostics are a **product feature**, not error handling: "this
/// `Content-Type` is malformed, here is how we read it" is exactly what a mail
/// debugging tool exists to say. A diagnostic never blocks storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// How serious the deviation is.
    pub severity: DiagnosticSeverity,
    /// Human-readable description.
    pub message: String,
    /// Where in the raw message it occurs, when known.
    pub range: Option<ByteRange>,
}

impl ParseDiagnostic {
    /// Creates a warning-level diagnostic.
    #[must_use]
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            message: message.into(),
            range: None,
        }
    }

    /// Creates an error-level diagnostic.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            message: message.into(),
            range: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_range_rejects_inverted_bounds() {
        assert!(ByteRange::new(10, 5).is_none());
        assert!(ByteRange::new(5, 5).is_some(), "empty ranges are valid");
    }

    #[test]
    fn byte_range_slices_exactly() {
        let raw = b"Hello, world";
        let range = ByteRange::new(7, 12).unwrap_or(ByteRange::empty_at(0));
        assert_eq!(range.slice(raw), Some(&b"world"[..]));
        assert_eq!(range.len(), 5);
    }

    #[test]
    fn out_of_bounds_slice_returns_none_rather_than_panicking() {
        let raw = b"short";
        let range = ByteRange::new(0, 100).unwrap_or(ByteRange::empty_at(0));
        assert_eq!(range.slice(raw), None);
        assert!(!range.is_within(raw.len()));
    }

    #[test]
    fn part_paths_render_in_imap_form() {
        assert_eq!(PartPath::root().to_string(), "");
        assert_eq!(PartPath::from_components(vec![2, 1]).to_string(), "2.1");
    }

    #[test]
    fn part_paths_round_trip() {
        for text in ["", "1", "2.1", "10.3.7"] {
            let parsed = PartPath::parse(text).unwrap_or_else(PartPath::root);
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn part_paths_reject_invalid_components() {
        assert!(PartPath::parse("0").is_none(), "components are 1-based");
        assert!(PartPath::parse("1.x").is_none());
        assert!(PartPath::parse("-1").is_none());
    }

    #[test]
    fn child_and_parent_are_inverse() {
        let parent = PartPath::from_components(vec![2]);
        let child = parent.child(1);
        assert_eq!(child.to_string(), "2.1");
        assert_eq!(child.parent(), Some(parent));
        assert_eq!(PartPath::root().parent(), None);
    }

    #[test]
    fn transfer_encoding_parses_case_insensitively() {
        assert_eq!(TransferEncoding::parse("BASE64"), TransferEncoding::Base64);
        assert_eq!(
            TransferEncoding::parse(" Quoted-Printable "),
            TransferEncoding::QuotedPrintable
        );
        assert_eq!(TransferEncoding::parse("uuencode"), TransferEncoding::Other);
    }
}
