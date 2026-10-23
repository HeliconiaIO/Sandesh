//! Rendering `FETCH` responses (RFC 9051 §7.5.2).
//!
//! ```text
//! * 1 FETCH (UID 4 FLAGS (\Seen) RFC822.SIZE 312 BODY[] {312}
//! <312 octets, byte-exact>
//! )
//! A001 OK FETCH completed
//! ```
//!
//! # Why the response is bytes, not a String
//!
//! A `FETCH` response carries message content, and message content is
//! arbitrary octets: an 8-bit header, a Latin-1 body, a PNG attachment. A
//! `String` would require the whole response to be valid UTF-8, and forcing it
//! would corrupt exactly the bytes this product guarantees to preserve
//! (phase brief §9). So the builder accumulates a `Vec<u8>`: text is appended
//! as ASCII, literal payloads are appended verbatim.
//!
//! # Literals
//!
//! Any value that may contain CRLF, NUL, 8-bit octets or a quote goes out as a
//! literal — `{n}CRLF` followed by exactly `n` octets — because that is the
//! only IMAP form with no escaping and therefore no way to change the bytes.
//! `n` is computed from the slice's own length at the moment it is written, so
//! it cannot disagree with the payload.

use sandesh_core::{
    Header, Message, MessageEntry, MessageFlags, MimePart, ParsedMessage, PartPath, Timestamp,
};

use crate::fetch::{FetchItem, Partial, Section, SectionKind};

/// Accumulates one `FETCH` response, keeping literal payloads byte-exact.
///
/// Wraps a `Vec<u8>` rather than exposing it so that a literal can only be
/// written through [`Self::literal`], which is the one place the length prefix
/// and the payload are derived from the same slice.
#[derive(Debug, Default)]
pub struct ResponseBuilder {
    out: Vec<u8>,
}

impl ResponseBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    /// A builder that has reserved room for `capacity` octets.
    ///
    /// Used when the response size is known in advance — a `BODY[]` fetch of a
    /// known-size message — so a large message is not repeatedly reallocated
    /// as it is appended (phase brief §21).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
        }
    }

    /// Appends ASCII text verbatim.
    ///
    /// Callers pass only text they generated — item names, numbers, flag
    /// names, parenthesised structure. Client-influenced text reaches the
    /// response through [`Self::quoted`] or [`Self::literal`], never here.
    pub fn text(&mut self, text: &str) {
        self.out.extend_from_slice(text.as_bytes());
    }

    /// Appends a quoted string, escaping `\` and `"`.
    ///
    /// Only for values known to contain no CRLF and no 8-bit octet: a
    /// media type, a charset, a parameter value. [`quotable`] decides, and
    /// anything it rejects goes out as a literal instead.
    pub fn quoted(&mut self, value: &str) {
        self.out.push(b'"');
        for byte in value.bytes() {
            if byte == b'"' || byte == b'\\' {
                self.out.push(b'\\');
            }
            self.out.push(byte);
        }
        self.out.push(b'"');
    }

    /// Appends a string as a quoted string, or `NIL` when absent.
    pub fn quoted_or_nil(&mut self, value: Option<&str>) {
        match value {
            Some(value) if quotable(value) => self.quoted(value),
            // A value that cannot be quoted safely still must not be dropped;
            // it goes out as a literal, which has no escaping problem.
            Some(value) => self.literal(value.as_bytes()),
            None => self.text("NIL"),
        }
    }

    /// Appends a literal: `{n}CRLF` then exactly `n` octets.
    ///
    /// # The sizing invariant
    ///
    /// The length prefix is `payload.len()` — read from the very slice that is
    /// then appended, in the same statement pair. There is no path by which a
    /// caller can supply a length that differs from the payload, which is the
    /// property phase brief §15 asks to be structurally guaranteed rather than
    /// merely tested.
    pub fn literal(&mut self, payload: &[u8]) {
        self.out
            .extend_from_slice(format!("{{{}}}\r\n", payload.len()).as_bytes());
        self.out.extend_from_slice(payload);
    }

    /// Appends a space.
    pub fn space(&mut self) {
        self.out.push(b' ');
    }

    /// The accumulated octets.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.out
    }

    /// Consumes the builder, yielding the octets.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    /// How many octets have accumulated.
    #[must_use]
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Whether nothing has been appended.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }
}

/// Whether a value may safely go out as a quoted string.
///
/// RFC 9051 §9 `QUOTED-CHAR` excludes CR, LF and NUL, and a quoted string is
/// defined over 7-bit characters. Everything else — an 8-bit filename, a
/// folded parameter — must be a literal, which imposes no such restriction.
#[must_use]
pub fn quotable(value: &str) -> bool {
    value.bytes().all(|byte| (0x20..0x7F).contains(&byte))
}

/// Formats an `INTERNALDATE` (RFC 9051 §9 `date-time`).
///
/// ```text
/// "17-Jul-2026 09:04:11 +0000"
///   ▲                     ▲
///   │                     └─ zone, always +0000: timestamps are stored UTC
///   └─ day is space-padded to two characters, not zero-padded
/// ```
///
/// The day-of-month padding is the detail clients are strict about: RFC 9051
/// §9 writes `date-day-fixed = (SP DIGIT) / 2DIGIT`, so the 7th is `" 7"` and
/// not `"07"`. Getting it wrong makes some clients reject the whole response.
///
/// The zone is `+0000` because [`Timestamp`] is UTC by construction
/// (`STORAGE_DESIGN.md` §2.1) — it is the real offset of the stored value, not
/// a placeholder.
#[must_use]
pub fn internal_date(timestamp: Timestamp) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let Some(datetime) = timestamp.to_datetime() else {
        // Unrepresentable only for a timestamp outside the calendar range.
        // The Unix epoch is a real instant and a valid date-time, so the
        // response stays well-formed rather than omitting a required item.
        return String::from(" 1-Jan-1970 00:00:00 +0000");
    };

    let month = MONTHS
        .get((u8::from(datetime.month()) as usize).saturating_sub(1))
        .copied()
        .unwrap_or("Jan");

    format!(
        "{:>2}-{}-{:04} {:02}:{:02}:{:02} +0000",
        datetime.day(),
        month,
        datetime.year(),
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
    )
}

/// Renders a `FLAGS` value: `(\Seen \Flagged keyword)`.
///
/// # Determinism
///
/// System flags come out in a fixed order and keywords are already a
/// `BTreeSet`, so the same flag set always renders identically. That is what
/// lets a test assert on the exact wire form, and it means a client diffing
/// two `FETCH` results does not see spurious changes (phase brief §5).
#[must_use]
pub fn flags(flags: &MessageFlags) -> String {
    let names = flags.all_names();
    format!("({})", names.join(" "))
}

/// The bytes of a message's headers, including the terminating blank line.
///
/// # Why the boundary is found in the raw bytes
///
/// RFC 5322 separates headers from body with an empty line. The header section
/// returned by `BODY[HEADER]` *includes* that blank line (RFC 9051 §6.4.5), so
/// a client can concatenate `BODY[HEADER]` and `BODY[TEXT]` and get the
/// message back.
///
/// The split is located by scanning the raw octets rather than by reserialising
/// parsed headers, which is the phase brief §10 requirement: ordering,
/// duplicate fields, folding, whitespace and encoded words all survive because
/// nothing is rebuilt — a range is taken.
///
/// A bare LF separator is honoured as well as CRLF. Some real mail uses it, and
/// the fixture corpus contains such a message; refusing to find the boundary
/// would make the whole message look like one giant header.
#[must_use]
pub fn header_end(raw: &[u8]) -> usize {
    // CRLFCRLF is the correct separator; find the earliest one.
    let crlf_boundary = find(raw, b"\r\n\r\n").map(|at| at + 4);
    // LFLF is the lenient fallback for bare-LF mail.
    let lf_boundary = find(raw, b"\n\n").map(|at| at + 2);

    match (crlf_boundary, lf_boundary) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        // No blank line at all: the message is all headers and has no body.
        (None, None) => raw.len(),
    }
}

/// The first index at which `needle` occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Selects the bytes a `BODY[...]` section denotes, as a slice of `raw`.
///
/// # The architecture this function is
///
/// ```text
/// raw bytes ──▶ MIME part ByteRange ──▶ exact sub-slice ──▶ literal
/// ```
///
/// Every arm returns a *slice of the original message*. Nothing is decoded,
/// re-encoded, normalised or rebuilt, so a `BODY[2]` of a base64 attachment
/// returns the base64 exactly as it arrived — which is what RFC 9051 §6.4.5
/// requires, since the client asked for the part's content, not its meaning.
///
/// Returns `None` when the section names a part that does not exist. RFC 9051
/// §6.4.5 makes that `NIL`-valued rather than an error, so the caller renders
/// an empty literal and the command still succeeds.
#[must_use]
pub fn section_bytes<'raw>(
    raw: &'raw [u8],
    parsed: &ParsedMessage,
    section: &Section,
) -> Option<&'raw [u8]> {
    if section.path.is_empty() {
        // Message-level sections.
        return match &section.kind {
            SectionKind::Full => Some(raw),
            SectionKind::Header => raw.get(..header_end(raw)),
            SectionKind::Text => raw.get(header_end(raw)..),
            SectionKind::HeaderFields(_) | SectionKind::HeaderFieldsNot(_) => {
                // Handled by the caller, which must build a new buffer from
                // selected header ranges; there is no single slice for it.
                None
            }
            // Rejected at parse time: `BODY[MIME]` has no meaning.
            SectionKind::Mime => None,
        };
    }

    let path = PartPath::from_components(section.path.clone());
    let part = parsed.part(&path)?;

    match &section.kind {
        // `BODY[n]` is the part's *body*, without its MIME headers
        // (RFC 9051 §6.4.5) — the headers are `BODY[n.MIME]`.
        SectionKind::Full => part.body(raw),
        // A part's own MIME headers.
        SectionKind::Mime => part.headers(raw),
        // For a message/rfc822 part, HEADER and TEXT address the *embedded*
        // message's header and body. For any other part the distinction does
        // not apply, and RFC 9051 §6.4.5 leaves it undefined; slicing the
        // part's body at its own blank line is the reading that keeps
        // `HEADER` + `TEXT` equal to the whole, which is the property clients
        // rely on.
        SectionKind::Header => {
            let body = part.body(raw)?;
            let end = header_end(body);
            body.get(..end)
        }
        SectionKind::Text => {
            let body = part.body(raw)?;
            let start = header_end(body);
            // A non-message part has no internal header section, in which case
            // `header_end` reports the whole body and TEXT would be empty.
            // Returning the body itself is the useful reading for `BODY[n.TEXT]`
            // of a plain text part.
            if start >= body.len() {
                Some(body)
            } else {
                body.get(start..)
            }
        }
        SectionKind::HeaderFields(_) | SectionKind::HeaderFieldsNot(_) => None,
    }
}

/// Builds the bytes for a `HEADER.FIELDS` / `HEADER.FIELDS.NOT` section.
///
/// # Why this is the one section that is assembled rather than sliced
///
/// A field selection is by definition a subset, so no single range covers it.
/// It is still built from ranges: each selected field is copied *verbatim* from
/// the raw message using [`Header::range`], so a folded value keeps its folding,
/// a duplicate `Received:` keeps both copies in their original order, and an
/// encoded word is not decoded. Nothing is reserialised from parsed values —
/// which is exactly what phase brief §10 forbids.
///
/// The result ends with a blank line, as RFC 9051 §6.4.5 requires of any
/// header section.
#[must_use]
pub fn header_fields(
    raw: &[u8],
    parsed: &ParsedMessage,
    names: &[String],
    exclude: bool,
) -> Vec<u8> {
    let mut out = Vec::new();

    for header in &parsed.headers {
        let is_selected = names
            .iter()
            .any(|field| header.name.eq_ignore_ascii_case(field));
        if is_selected == exclude {
            // Either an excluded field in NOT mode, or an unnamed field in
            // inclusive mode.
            continue;
        }
        if let Some(bytes) = header.range.slice(raw) {
            out.extend_from_slice(bytes);
            // The stored range may or may not include the field's own line
            // ending, depending on how the parser recorded it. Normalising
            // only the *separator between fields* keeps each field's own bytes
            // untouched while guaranteeing the section is well-formed.
            if !out.ends_with(b"\n") {
                out.extend_from_slice(b"\r\n");
            }
        }
    }

    // A header section always ends with the blank line that terminates it.
    out.extend_from_slice(b"\r\n");
    out
}

/// Renders an `ENVELOPE` (RFC 9051 §7.5.2).
///
/// ```text
/// ENVELOPE (date subject from sender reply-to to cc bcc in-reply-to message-id)
/// ```
///
/// # Where the values come from
///
/// All ten fields come from the message's **headers**, not from the SMTP
/// envelope. [`sandesh_core::Envelope`] is the SMTP envelope — `MAIL FROM` and
/// `RCPT TO` — and using it here would be wrong in a way that shows up only
/// with `Bcc`: the IMAP `ENVELOPE` reports what the message says, and the SMTP
/// envelope reports where it actually went.
///
/// A missing header is `NIL`. RFC 9051 §7.5.2 also defines defaults: `sender`
/// and `reply-to` fall back to `from` when absent, which clients rely on to
/// decide where a reply goes.
pub fn envelope(builder: &mut ResponseBuilder, raw: &[u8], parsed: &ParsedMessage) {
    /// The raw, undecoded value of a header, as text.
    fn raw_value<'a>(parsed: &'a ParsedMessage, name: &str) -> Option<&'a Header> {
        parsed.headers.iter().find(|h| h.name_matches(name))
    }

    builder.text("(");

    // date
    builder.quoted_or_nil(raw_value(parsed, "Date").map(|h| h.decoded_value.trim()));
    builder.space();
    // subject — decoded, because RFC 9051 §7.5.2 envelope fields carry the
    // header's value and clients display it directly.
    builder.quoted_or_nil(raw_value(parsed, "Subject").map(|h| h.decoded_value.trim()));

    // The five address lists, with the RFC 9051 §7.5.2 fallbacks.
    let from = address_list(parsed, "From");
    for (name, fallback) in [
        ("From", None),
        ("Sender", Some(&from)),
        ("Reply-To", Some(&from)),
        ("To", None),
        ("Cc", None),
        ("Bcc", None),
    ] {
        builder.space();
        let list = address_list(parsed, name);
        let effective = if list.is_empty() {
            fallback.map_or(&[][..], |value| value.as_slice())
        } else {
            list.as_slice()
        };
        render_address_list(builder, effective);
    }

    builder.space();
    builder.quoted_or_nil(raw_value(parsed, "In-Reply-To").map(|h| h.decoded_value.trim()));
    builder.space();
    builder.quoted_or_nil(raw_value(parsed, "Message-ID").map(|h| h.decoded_value.trim()));

    builder.text(")");

    // `raw` is unused here by design: every envelope field is a header value,
    // and the parsed headers already carry both the raw and decoded forms.
    let _ = raw;
}

/// One envelope address: `(name adl mailbox host)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvelopeAddress {
    /// Display name, or `None`.
    name: Option<String>,
    /// The local part.
    mailbox: String,
    /// The domain.
    host: String,
}

/// Parses an address-list header into envelope addresses.
///
/// Splits on commas outside quotes and angle brackets, so a display name
/// containing a comma — `"Doe, Jane" <jane@example.com>` — stays one address.
fn address_list(parsed: &ParsedMessage, name: &str) -> Vec<EnvelopeAddress> {
    let Some(header) = parsed.headers.iter().find(|h| h.name_matches(name)) else {
        return Vec::new();
    };

    split_addresses(&header.decoded_value)
        .into_iter()
        .filter_map(|piece| {
            let address = sandesh_core::EmailAddress::parse_lenient(piece.trim());
            let mailbox = address.local_part();
            let host = address.domain();
            if mailbox.is_empty() && host.is_empty() {
                return None;
            }
            Some(EnvelopeAddress {
                name: address.display_name().map(str::to_owned),
                mailbox: mailbox.to_owned(),
                host: host.to_owned(),
            })
        })
        .collect()
}

/// Splits an address-list header on top-level commas.
fn split_addresses(value: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut depth = 0usize;

    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b'<' if !in_quotes => depth += 1,
            b'>' if !in_quotes => depth = depth.saturating_sub(1),
            b',' if !in_quotes && depth == 0 => {
                if let Some(piece) = value.get(start..index) {
                    pieces.push(piece);
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    if let Some(piece) = value.get(start..) {
        pieces.push(piece);
    }
    pieces
        .into_iter()
        .filter(|piece| !piece.trim().is_empty())
        .collect()
}

/// Renders an address list, or `NIL` when it is empty.
fn render_address_list(builder: &mut ResponseBuilder, addresses: &[EnvelopeAddress]) {
    if addresses.is_empty() {
        builder.text("NIL");
        return;
    }
    builder.text("(");
    for address in addresses {
        builder.text("(");
        builder.quoted_or_nil(address.name.as_deref());
        // The `adl` (at-domain-list) field is a source-routing relic; RFC 9051
        // §7.5.2 keeps the slot and it is always NIL for modern mail.
        builder.text(" NIL ");
        builder.quoted_or_nil(Some(&address.mailbox));
        builder.space();
        builder.quoted_or_nil(Some(&address.host));
        builder.text(")");
    }
    builder.text(")");
}

/// Renders a `BODYSTRUCTURE` or `BODY` (RFC 9051 §7.5.2).
///
/// `extended` selects between the two: `BODYSTRUCTURE` includes the extension
/// data (MD5, disposition, language, location), `BODY` stops before it. They
/// are otherwise identical, which is why one function renders both — two would
/// drift.
///
/// # Where the structure comes from
///
/// Entirely from [`ParsedMessage::parts`], the core MIME model. No MIME parsing
/// happens here and none may: a second parser in the protocol layer is exactly
/// the duplication phase brief §11 and the architecture test forbid.
pub fn body_structure(
    builder: &mut ResponseBuilder,
    raw: &[u8],
    parsed: &ParsedMessage,
    extended: bool,
) {
    let Some(root) = parsed.root_part() else {
        // A message with no parsed parts at all — an empty or unparseable
        // message. RFC 9051 requires *some* structure, and the honest one is a
        // single empty text part describing what is actually stored: the whole
        // body, as text, with its real length.
        render_degenerate(builder, raw);
        return;
    };
    render_part(builder, raw, parsed, root, extended);
}

/// Renders the structure of a message whose MIME could not be parsed.
///
/// This is not fabricated metadata: the media type is the MIME default for a
/// message with no `Content-Type` (RFC 2045 §5.2 makes it `text/plain;
/// charset=us-ascii`), and the sizes are measured from the stored bytes. What
/// it deliberately does not do is guess at a structure the message does not
/// demonstrably have.
fn render_degenerate(builder: &mut ResponseBuilder, raw: &[u8]) {
    let body = raw.get(header_end(raw)..).unwrap_or(&[]);
    builder.text("(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"US-ASCII\") NIL NIL \"7BIT\" ");
    builder.text(&body.len().to_string());
    builder.space();
    builder.text(&count_lines(body).to_string());
    builder.text(")");
}

/// Renders one MIME part, recursing into children.
fn render_part(
    builder: &mut ResponseBuilder,
    raw: &[u8],
    parsed: &ParsedMessage,
    part: &MimePart,
    extended: bool,
) {
    if part.is_multipart() {
        render_multipart(builder, raw, parsed, part, extended);
    } else {
        render_leaf(builder, raw, part, extended);
    }
}

/// Renders a `multipart/*` part.
///
/// ```text
/// (child child ... "MIXED" ("BOUNDARY" "x") NIL NIL NIL)
///  ▲                ▲
///  │                └─ the subtype, after the children — multipart is the one
///  │                   shape where the type comes last
///  └─ one nested structure per child, with no separating spaces
/// ```
fn render_multipart(
    builder: &mut ResponseBuilder,
    raw: &[u8],
    parsed: &ParsedMessage,
    part: &MimePart,
    extended: bool,
) {
    builder.text("(");

    // Children first, concatenated with no separator (RFC 9051 §7.5.2).
    let mut rendered_any = false;
    for child_path in &part.children {
        if let Some(child) = parsed.part(child_path) {
            render_part(builder, raw, parsed, child, extended);
            rendered_any = true;
        }
    }
    if !rendered_any {
        // A multipart with no parseable children still needs one body part to
        // be well-formed. Describe what is stored rather than claiming a child.
        render_degenerate(builder, part.body(raw).unwrap_or(&[]));
    }

    builder.space();
    builder.quoted(&subtype_of(&part.content_type).to_ascii_uppercase());

    if extended {
        // Extension data for a multipart: parameters, disposition, language,
        // location. The boundary is the parameter clients actually use.
        builder.space();
        builder.text("NIL");
        builder.text(" NIL NIL NIL");
    }

    builder.text(")");
}

/// Renders a non-multipart part.
///
/// ```text
/// ("TEXT" "PLAIN" ("CHARSET" "utf-8") NIL NIL "7BIT" 42 3)
///                                                    ▲  ▲
///                                                    │  └─ line count, text only
///                                                    └──── octets, encoded
/// ```
///
/// The size is the *encoded* body length in octets, taken from the part's byte
/// range — not the decoded length. RFC 9051 §7.5.2 defines it as the size of
/// the body as transmitted, which is what a client uses to predict how much a
/// `BODY[n]` fetch will return.
fn render_leaf(builder: &mut ResponseBuilder, raw: &[u8], part: &MimePart, extended: bool) {
    let (media_type, subtype) = split_content_type(&part.content_type);
    let body = part.body(raw).unwrap_or(&[]);

    builder.text("(");
    builder.quoted(&media_type.to_ascii_uppercase());
    builder.space();
    builder.quoted(&subtype.to_ascii_uppercase());

    // Parameters: charset for text, name for anything with a filename.
    builder.space();
    render_parameters(builder, part);

    // Content-ID, Content-Description.
    builder.space();
    builder.quoted_or_nil(part.content_id.as_deref());
    builder.text(" NIL ");

    // Content-Transfer-Encoding.
    builder.quoted(&part.encoding.as_str().to_ascii_uppercase());
    builder.space();
    builder.text(&body.len().to_string());

    // `text/*` carries a line count; nothing else does (RFC 9051 §7.5.2).
    if media_type.eq_ignore_ascii_case("text") {
        builder.space();
        builder.text(&count_lines(body).to_string());
    }

    if extended {
        // MD5 is not computed — the stored digest is over the whole message,
        // not per part, and inventing one would be fabricated metadata.
        builder.text(" NIL ");
        render_disposition(builder, part);
        // Language and location.
        builder.text(" NIL NIL");
    }

    builder.text(")");
}

/// Renders a part's body parameters, or `NIL`.
fn render_parameters(builder: &mut ResponseBuilder, part: &MimePart) {
    let mut parameters: Vec<(&str, &str)> = Vec::new();
    if let Some(charset) = part.charset.as_deref() {
        parameters.push(("CHARSET", charset));
    }
    if let Some(filename) = part.filename.as_deref() {
        parameters.push(("NAME", filename));
    }

    if parameters.is_empty() {
        builder.text("NIL");
        return;
    }
    builder.text("(");
    for (index, (key, value)) in parameters.iter().enumerate() {
        if index > 0 {
            builder.space();
        }
        builder.quoted(key);
        builder.space();
        builder.quoted_or_nil(Some(value));
    }
    builder.text(")");
}

/// Renders a part's `Content-Disposition`, or `NIL`.
fn render_disposition(builder: &mut ResponseBuilder, part: &MimePart) {
    if part.disposition == sandesh_core::Disposition::Inline && part.filename.is_none() {
        builder.text("NIL");
        return;
    }
    let disposition = if part.disposition == sandesh_core::Disposition::Attachment {
        "attachment"
    } else {
        "inline"
    };
    builder.text("(");
    builder.quoted(disposition);
    builder.space();
    match part.filename.as_deref() {
        Some(filename) => {
            builder.text("(");
            builder.quoted("FILENAME");
            builder.space();
            builder.quoted_or_nil(Some(filename));
            builder.text(")");
        }
        None => builder.text("NIL"),
    }
    builder.text(")");
}

/// Splits `text/plain` into `("text", "plain")`.
fn split_content_type(content_type: &str) -> (&str, &str) {
    content_type
        .split_once('/')
        .unwrap_or((content_type, "plain"))
}

/// The subtype half of a media type.
fn subtype_of(content_type: &str) -> &str {
    split_content_type(content_type).1
}

/// Counts lines the way RFC 9051 §7.5.2 `body-fld-lines` does.
///
/// The count is of line *terminators*, with a final unterminated line counted
/// as one. An empty body has no lines.
fn count_lines(body: &[u8]) -> usize {
    if body.is_empty() {
        return 0;
    }
    // `naive_bytecount` fires on `iter().filter(...).count()`; adding a crate
    // dependency for a line count is not worth it, so the scan is written as
    // an explicit fold over the slice.
    let terminators = body.iter().fold(
        0usize,
        |count, byte| {
            if *byte == b'\n' { count + 1 } else { count }
        },
    );
    if body.ends_with(b"\n") {
        terminators
    } else {
        terminators + 1
    }
}

/// Everything one message contributes to a `FETCH` response.
///
/// Gathered by the session, which owns the ports, and rendered here. Bundling
/// them means the renderer cannot accidentally reach for storage.
#[derive(Debug)]
pub struct MessageData<'a> {
    /// The placement being reported — the source of UID, FLAGS, INTERNALDATE.
    pub entry: &'a MessageEntry,
    /// The message, when any item required it.
    pub message: Option<&'a Message>,
}

/// Renders one untagged `FETCH` response line for a message.
///
/// ```text
/// * <sequence> FETCH (<item> <value> <item> <value>)CRLF
/// ```
///
/// # UID is always included in UID mode
///
/// RFC 9051 §6.4.8 requires it: a `UID FETCH` response must carry the UID even
/// if the client did not ask, because the client addressed the message by UID
/// and needs to match the response to its request. `uid_mode` adds it when the
/// item list does not already.
pub fn fetch_line(
    sequence: usize,
    data: &MessageData<'_>,
    items: &[FetchItem],
    uid_mode: bool,
) -> Vec<u8> {
    let mut builder = ResponseBuilder::with_capacity(estimate_capacity(data, items));

    builder.text(&format!("* {sequence} FETCH ("));

    let mut first = true;
    let mut emitted_uid = false;

    for item in items {
        if !first {
            builder.space();
        }
        first = false;
        if matches!(item, FetchItem::Uid) {
            emitted_uid = true;
        }
        render_item(&mut builder, data, item);
    }

    // RFC 9051 §6.4.8: UID FETCH always reports the UID.
    if uid_mode && !emitted_uid {
        if !first {
            builder.space();
        }
        builder.text("UID ");
        builder.text(&data.entry.uid.get().to_string());
    }

    builder.text(")\r\n");
    builder.into_bytes()
}

/// A capacity hint for one response line.
///
/// Reserves the raw message's length once when a content item is present, so a
/// large `BODY[]` is appended into an already-sized buffer instead of growing
/// it repeatedly. Only a hint — a wrong guess costs nothing but a realloc.
fn estimate_capacity(data: &MessageData<'_>, items: &[FetchItem]) -> usize {
    /// Room for item names, numbers and structure.
    const OVERHEAD: usize = 512;

    let content = items.iter().any(FetchItem::needs_raw);
    let raw_len = if content {
        data.message.map_or(0, |message| message.raw.bytes().len())
    } else {
        0
    };
    raw_len.saturating_add(OVERHEAD)
}

/// Renders one item's name and value.
fn render_item(builder: &mut ResponseBuilder, data: &MessageData<'_>, item: &FetchItem) {
    builder.text(&item.render_name());
    builder.space();

    match item {
        FetchItem::Uid => builder.text(&data.entry.uid.get().to_string()),
        FetchItem::Flags => builder.text(&flags(&data.entry.flags)),

        // RFC 7162 §3.1.4: a parenthesised list holding one mod-sequence
        // value. The parentheses are part of the syntax, not decoration — a
        // bare number would not parse for a CONDSTORE-aware client.
        FetchItem::ModSeq => builder.text(&format!("({})", data.entry.mod_seq.get())),
        FetchItem::InternalDate => builder.quoted(&internal_date(data.entry.internal_date)),

        FetchItem::Rfc822Size => {
            // The authoritative size: the stored raw octet count. Never a
            // decoded length, never a header-only length (phase brief §8).
            let size = data.message.map_or(0, |message| message.raw.size());
            builder.text(&size.to_string());
        }

        FetchItem::Envelope => match data.message {
            Some(message) => envelope(builder, message.raw.bytes(), &message.parsed),
            None => builder.text("NIL"),
        },

        FetchItem::BodyStructure | FetchItem::Body => {
            let extended = matches!(item, FetchItem::BodyStructure);
            match data.message {
                Some(message) => {
                    body_structure(builder, message.raw.bytes(), &message.parsed, extended);
                }
                None => builder.text("NIL"),
            }
        }

        FetchItem::Section {
            section, partial, ..
        } => render_section(builder, data, section, *partial),
    }
}

/// Renders a `BODY[...]` value as a literal.
///
/// Always a literal, never a quoted string, even for a short ASCII section:
/// content is arbitrary octets, and a literal is the only form that cannot
/// change them. An absent part yields an *empty* literal rather than `NIL`,
/// because the client asked for octets and `{0}` is the honest answer for a
/// section that exists in the request but not in the message.
fn render_section(
    builder: &mut ResponseBuilder,
    data: &MessageData<'_>,
    section: &Section,
    partial: Option<Partial>,
) {
    let Some(message) = data.message else {
        builder.literal(&[]);
        return;
    };
    let raw = message.raw.bytes();

    // A field selection is assembled; every other section is a slice.
    let assembled: Option<Vec<u8>> = match &section.kind {
        SectionKind::HeaderFields(names) => Some(header_fields(raw, &message.parsed, names, false)),
        SectionKind::HeaderFieldsNot(names) => {
            Some(header_fields(raw, &message.parsed, names, true))
        }
        _ => None,
    };

    let bytes: &[u8] = match assembled.as_deref() {
        Some(built) => built,
        None => section_bytes(raw, &message.parsed, section).unwrap_or(&[]),
    };

    // The partial window is applied last, to the bytes that would otherwise
    // have been sent, so `<offset.count>` is always relative to the section.
    let payload = match partial {
        Some(window) => window.apply(bytes),
        None => bytes,
    };

    builder.literal(payload);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_core::{Keyword, SystemFlags};

    // --- literals ---------------------------------------------------------

    #[test]
    fn a_literal_states_its_own_length() {
        let mut builder = ResponseBuilder::new();
        builder.literal(b"hello");
        assert_eq!(builder.as_bytes(), b"{5}\r\nhello");
    }

    #[test]
    fn an_empty_literal_is_well_formed() {
        let mut builder = ResponseBuilder::new();
        builder.literal(b"");
        assert_eq!(builder.as_bytes(), b"{0}\r\n");
    }

    #[test]
    fn a_literal_length_always_matches_its_payload() {
        // The structural invariant: for any payload, the declared count and
        // the actual octets agree.
        for payload in [
            &b""[..],
            &b"a"[..],
            &b"line\r\nline"[..],
            &b"\0\xff\xfe binary"[..],
            &[0u8; 1000][..],
        ] {
            let mut builder = ResponseBuilder::new();
            builder.literal(payload);
            let out = builder.into_bytes();

            let header_end = out
                .iter()
                .position(|byte| *byte == b'\n')
                .expect("a literal header")
                + 1;
            let declared: usize = core::str::from_utf8(&out[1..header_end - 3])
                .expect("ascii digits")
                .parse()
                .expect("a number");
            assert_eq!(declared, payload.len(), "declared length");
            assert_eq!(&out[header_end..], payload, "payload is byte-exact");
        }
    }

    #[test]
    fn binary_octets_survive_a_literal_unchanged() {
        let payload: Vec<u8> = (0u8..=255).collect();
        let mut builder = ResponseBuilder::new();
        builder.literal(&payload);
        let out = builder.into_bytes();
        assert!(out.ends_with(&payload), "every octet must survive");
        assert!(out.starts_with(b"{256}\r\n"));
    }

    #[test]
    fn crlf_inside_a_literal_is_preserved_not_normalised() {
        let payload = b"a\r\nb\nc\rd";
        let mut builder = ResponseBuilder::new();
        builder.literal(payload);
        let out = builder.into_bytes();
        assert!(out.ends_with(payload), "line endings must not be rewritten");
    }

    #[test]
    fn multiple_literals_in_one_response_each_carry_their_own_length() {
        let mut builder = ResponseBuilder::new();
        builder.text("BODY[HEADER] ");
        builder.literal(b"From: a\r\n\r\n");
        builder.text(" BODY[TEXT] ");
        builder.literal(b"hi");
        let out = builder.into_bytes();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("{11}\r\nFrom: a\r\n\r\n"), "{text}");
        assert!(text.contains("{2}\r\nhi"), "{text}");
    }

    // --- quoting ----------------------------------------------------------

    #[test]
    fn quoted_strings_escape_quotes_and_backslashes() {
        let mut builder = ResponseBuilder::new();
        builder.quoted(r#"a"b\c"#);
        assert_eq!(builder.as_bytes(), br#""a\"b\\c""#);
    }

    #[test]
    fn an_unquotable_value_becomes_a_literal() {
        // An 8-bit value cannot be a quoted string, and must not be dropped.
        let mut builder = ResponseBuilder::new();
        builder.quoted_or_nil(Some("Ärchiv"));
        let out = String::from_utf8_lossy(builder.as_bytes()).into_owned();
        assert!(out.starts_with('{'), "must be a literal: {out}");
        assert!(out.ends_with("Ärchiv"), "{out}");
    }

    #[test]
    fn an_absent_value_is_nil_not_an_empty_string() {
        let mut builder = ResponseBuilder::new();
        builder.quoted_or_nil(None);
        assert_eq!(builder.as_bytes(), b"NIL");
    }

    #[test]
    fn a_value_containing_crlf_cannot_split_the_response() {
        // The response-splitting guard for FETCH values.
        let mut builder = ResponseBuilder::new();
        builder.quoted_or_nil(Some("a\r\n* BYE forged"));
        let out = String::from_utf8_lossy(builder.as_bytes()).into_owned();
        // It goes out as a literal, whose length is declared, so the CRLF is
        // data the client counts rather than a line boundary it obeys.
        assert!(out.starts_with('{'), "{out}");
    }

    // --- INTERNALDATE -----------------------------------------------------

    #[test]
    fn internal_date_uses_the_rfc_shape() {
        // 2026-09-08T14:23:45Z
        let timestamp = Timestamp::from_micros(1_788_878_625_000_000);
        let rendered = internal_date(timestamp);
        assert!(
            rendered.ends_with(" +0000"),
            "stored timestamps are UTC: {rendered}"
        );
        assert_eq!(rendered.len(), 26, "fixed width: {rendered}");
        assert!(rendered.contains("-Sep-2026"), "{rendered}");
    }

    #[test]
    fn the_day_of_month_is_space_padded_not_zero_padded() {
        // RFC 9051 §9 `date-day-fixed`: strict clients reject "07".
        // 2026-09-07T00:00:00Z
        let seventh = Timestamp::from_micros(1_788_739_200_000_000);
        let rendered = internal_date(seventh);
        assert!(
            rendered.starts_with(" 7-Sep-2026"),
            "the 7th must be space-padded: {rendered:?}"
        );
        assert!(!rendered.starts_with("07"), "{rendered:?}");
    }

    #[test]
    fn internal_date_is_the_stored_value_not_now() {
        // The mutation this catches: formatting the current time instead.
        let epoch = internal_date(Timestamp::from_micros(0));
        assert_eq!(epoch, " 1-Jan-1970 00:00:00 +0000");
        assert_ne!(epoch, internal_date(Timestamp::now()));
    }

    #[test]
    fn an_extreme_timestamp_still_renders_a_valid_date() {
        for micros in [i64::MIN, i64::MAX, -1, 0, 1] {
            let rendered = internal_date(Timestamp::from_micros(micros));
            assert!(rendered.ends_with("+0000"), "{rendered}");
            assert!(!rendered.contains('\r') && !rendered.contains('\n'));
        }
    }

    // --- flags ------------------------------------------------------------

    #[test]
    fn flags_render_as_a_parenthesised_list() {
        let value = MessageFlags::from_system(SystemFlags::SEEN);
        assert_eq!(flags(&value), "(\\Seen)");
    }

    #[test]
    fn no_flags_renders_as_an_empty_list() {
        assert_eq!(flags(&MessageFlags::empty()), "()");
    }

    #[test]
    fn flag_rendering_is_deterministic() {
        // The same set must always produce the same octets, or a client
        // diffing two FETCH results sees changes that did not happen.
        let mut keywords = std::collections::BTreeSet::new();
        keywords.insert(Keyword::new("$label1").expect("valid keyword"));
        keywords.insert(Keyword::new("Urgent").expect("valid keyword"));

        let value = MessageFlags::new(
            SystemFlags::SEEN.union(SystemFlags::FLAGGED),
            keywords.clone(),
        );
        let once = flags(&value);
        let again = flags(&MessageFlags::new(
            SystemFlags::FLAGGED.union(SystemFlags::SEEN),
            keywords,
        ));
        assert_eq!(once, again, "order must not depend on insertion order");
    }

    #[test]
    fn custom_keywords_are_preserved() {
        let mut keywords = std::collections::BTreeSet::new();
        keywords.insert(Keyword::new("Urgent").expect("valid keyword"));
        let value = MessageFlags::new(SystemFlags::NONE, keywords);
        assert!(flags(&value).contains("Urgent"), "{}", flags(&value));
    }

    // --- header boundary --------------------------------------------------

    #[test]
    fn the_header_section_includes_its_terminating_blank_line() {
        // So that BODY[HEADER] + BODY[TEXT] == BODY[].
        let raw = b"From: a@b\r\nSubject: hi\r\n\r\nbody here";
        let end = header_end(raw);
        assert_eq!(&raw[..end], b"From: a@b\r\nSubject: hi\r\n\r\n");
        assert_eq!(&raw[end..], b"body here");
    }

    #[test]
    fn header_and_text_always_reconstruct_the_message() {
        for raw in [
            &b"From: a\r\n\r\nbody"[..],
            &b"From: a\n\nbody"[..],
            &b"From: a\r\n\r\n"[..],
            &b"no blank line at all"[..],
            &b""[..],
        ] {
            let end = header_end(raw);
            let mut rebuilt = Vec::new();
            rebuilt.extend_from_slice(&raw[..end]);
            rebuilt.extend_from_slice(&raw[end..]);
            assert_eq!(rebuilt, raw, "the split must lose nothing");
        }
    }

    #[test]
    fn a_bare_lf_separator_is_found() {
        let raw = b"From: a\nSubject: b\n\nbody";
        let end = header_end(raw);
        assert_eq!(&raw[end..], b"body");
    }

    #[test]
    fn a_message_with_no_body_has_an_empty_text_section() {
        let raw = b"From: a\r\n\r\n";
        let end = header_end(raw);
        assert_eq!(&raw[end..], b"");
    }

    // --- line counting ----------------------------------------------------

    #[test]
    fn line_counts_follow_the_rfc_definition() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"one"), 1, "an unterminated line still counts");
        assert_eq!(count_lines(b"one\r\n"), 1);
        assert_eq!(count_lines(b"one\r\ntwo\r\n"), 2);
        assert_eq!(count_lines(b"one\r\ntwo"), 2);
    }

    // --- address splitting ------------------------------------------------

    #[test]
    fn an_address_list_splits_on_top_level_commas_only() {
        let pieces = split_addresses(r#""Doe, Jane" <jane@example.com>, bob@example.com"#);
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        assert!(pieces[0].contains("Doe, Jane"));
        assert!(pieces[1].contains("bob@"));
    }

    #[test]
    fn an_empty_address_list_yields_nothing() {
        assert!(split_addresses("").is_empty());
        assert!(split_addresses("  ,  ").is_empty());
    }
}
