//! Adapter from `mail-parser` output to the Sandesh MIME model.
//!
//! Two responsibilities:
//!
//! 1. Translate `mail-parser`'s flat part list into IMAP part paths
//!    (`1`, `2.1`, …), which is what `FETCH BODY[<path>]` addresses.
//! 2. Capture byte offsets so every part remains a *range into the raw
//!    message* rather than a copy (ADR-003).
//!
//! **Leniency contract** (`CORE_SPEC.md` §8): parsing never panics, never
//! rejects, and never loses bytes. A message that will not parse is still
//! stored in full and remains retrievable; the failure is recorded as a
//! diagnostic, which is itself a product feature.

use crate::{
    Attachment, AttachmentId, ByteRange, DiagnosticSeverity, Disposition, Header, MimePart,
    ParseDiagnostic, ParsedMessage, PartPath, TransferEncoding,
};
use mail_parser::{Encoding, Message, MessageParser, MessagePart, MimeHeaders, PartType};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Version of this parsing logic.
///
/// Persisted alongside derived data; bumping it invalidates cached structure so
/// it is rebuilt from the untouched raw bytes.
pub const PARSE_VERSION: u32 = 1;

/// Runs `mail-parser`, treating a panic inside it as "could not be parsed".
///
/// `mail-parser` 0.11.9 panics ("Invalid part ID, could not find multipart")
/// on some malformed nested `multipart/report` messages, found by the
/// `mime_parser` fuzz target. Every received message passes through here, so a
/// panic would let any sender crash the connection handling it. Catching it
/// keeps the leniency contract above: the message is stored with a diagnostic.
fn run_parser(raw: &[u8]) -> Option<Message<'_>> {
    catch_unwind(AssertUnwindSafe(|| MessageParser::default().parse(raw)))
        .ok()
        .flatten()
}

/// Parses raw message bytes into derived structure.
///
/// Always returns a [`ParsedMessage`]. If the bytes cannot be parsed at all,
/// the result carries `parse_ok = false` plus a diagnostic, and the caller
/// stores the message regardless.
#[must_use]
pub fn parse(raw: &[u8]) -> ParsedMessage {
    let Some(parsed) = run_parser(raw) else {
        return ParsedMessage {
            headers: Vec::new(),
            parts: Vec::new(),
            text_parts: Vec::new(),
            html_parts: Vec::new(),
            attachments: Vec::new(),
            diagnostics: vec![ParseDiagnostic::error(
                "message could not be parsed as RFC 5322; raw bytes preserved",
            )],
            parse_ok: false,
            parse_version: PARSE_VERSION,
        };
    };

    let mut diagnostics = Vec::new();
    let headers = convert_headers(parsed.headers(), raw, &mut diagnostics);

    let mut parts = Vec::new();
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();

    // The root of mail-parser's part list is index 0; walk from there,
    // assigning IMAP paths as we descend.
    if !parsed.parts.is_empty() {
        walk(
            &parsed.parts,
            0,
            &PartPath::root(),
            raw,
            &mut parts,
            &mut text_parts,
            &mut html_parts,
            &mut diagnostics,
            0,
        );
    }

    let attachments = collect_attachments(&parts);

    if parts.is_empty() {
        diagnostics.push(ParseDiagnostic::warning("message contains no MIME parts"));
    }

    let parse_ok = !diagnostics
        .iter()
        .any(|d| d.severity == DiagnosticSeverity::Error);

    ParsedMessage {
        headers,
        parts,
        text_parts,
        html_parts,
        attachments,
        diagnostics,
        parse_ok,
        parse_version: PARSE_VERSION,
    }
}

/// Extracts the decoded bytes of the part at `target` from the raw message.
///
/// Unlike [`MimePart::body`], which returns the raw, possibly transfer-encoded
/// bytes (e.g. Base64 or Quoted-Printable) directly sliced from the wire data
/// for RFC-strict protocol commands like IMAP `FETCH BODY[...]`, this function
/// decodes the transfer encoding (and text charset) so the caller receives the
/// true payload (e.g. decoded HTML, plain text, or binary attachment).
#[must_use]
pub fn decoded_part_body(raw: &[u8], target: &PartPath) -> Option<Vec<u8>> {
    let parsed = run_parser(raw)?;
    if parsed.parts.is_empty() {
        return None;
    }
    find_decoded_part(&parsed.parts, 0, &PartPath::root(), target, 0)
}

fn find_decoded_part(
    all: &[MessagePart<'_>],
    index: usize,
    current_path: &PartPath,
    target: &PartPath,
    depth: usize,
) -> Option<Vec<u8>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let part = all.get(index)?;
    if current_path == target {
        return match &part.body {
            PartType::Text(text) | PartType::Html(text) => Some(text.as_bytes().to_vec()),
            PartType::Binary(bin) | PartType::InlineBinary(bin) => Some(bin.to_vec()),
            PartType::Message(msg) => Some(msg.raw_message.to_vec()),
            PartType::Multipart(_) => None,
        };
    }

    if let PartType::Multipart(ids) = &part.body {
        for (ordinal, &child_index) in ids.iter().enumerate() {
            let Ok(number) = u32::try_from(ordinal + 1) else {
                continue;
            };
            let child_path = current_path.child(number);
            if let Some(found) =
                find_decoded_part(all, child_index as usize, &child_path, target, depth + 1)
            {
                return Some(found);
            }
        }
    }

    None
}

/// Maximum MIME nesting depth.
///
/// A guard against pathological or adversarial messages: without it, a deeply
/// nested multipart could exhaust the stack. Parts below the limit are dropped
/// with a diagnostic rather than causing a failure.
const MAX_DEPTH: usize = 64;

#[allow(clippy::too_many_arguments)]
fn walk(
    all: &[MessagePart<'_>],
    index: usize,
    path: &PartPath,
    raw: &[u8],
    out: &mut Vec<MimePart>,
    text: &mut Vec<PartPath>,
    html: &mut Vec<PartPath>,
    diagnostics: &mut Vec<ParseDiagnostic>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        diagnostics.push(ParseDiagnostic::warning(format!(
            "MIME nesting deeper than {MAX_DEPTH} levels was truncated at part {path}"
        )));
        return;
    }
    let Some(part) = all.get(index) else {
        diagnostics.push(ParseDiagnostic::warning(format!(
            "MIME part index {index} referenced but absent"
        )));
        return;
    };

    match &part.body {
        PartType::Text(_) => text.push(path.clone()),
        PartType::Html(_) => html.push(path.clone()),
        _ => {}
    }

    let child_indices: Vec<usize> = match &part.body {
        PartType::Multipart(ids) => ids.iter().map(|id| *id as usize).collect(),
        _ => Vec::new(),
    };

    out.push(convert_part(part, path, raw, &child_indices, diagnostics));

    for (ordinal, child_index) in child_indices.into_iter().enumerate() {
        let Ok(number) = u32::try_from(ordinal + 1) else {
            continue;
        };
        walk(
            all,
            child_index,
            &path.child(number),
            raw,
            out,
            text,
            html,
            diagnostics,
            depth + 1,
        );
    }
}

/// Converts one `mail-parser` part into the domain representation.
///
/// Captures byte ranges rather than content: the body stays in the raw message
/// and is addressed by offset.
fn convert_part(
    part: &MessagePart<'_>,
    path: &PartPath,
    raw: &[u8],
    child_indices: &[usize],
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> MimePart {
    let header_range = range_or_empty(part.offset_header, part.offset_body, raw, diagnostics);
    let body_range = range_or_empty(part.offset_body, part.offset_end, raw, diagnostics);

    // Disposition comes from the Content-Disposition header. A part with a
    // filename but no explicit disposition is treated as an attachment, which
    // is what mail clients do in practice.
    let disposition = match part.content_disposition() {
        Some(cd) if cd.is_attachment() => Disposition::Attachment,
        Some(_) => Disposition::Inline,
        None => {
            if part.attachment_name().is_some() {
                Disposition::Attachment
            } else {
                Disposition::Inline
            }
        }
    };

    if part.is_encoding_problem {
        diagnostics.push(ParseDiagnostic {
            severity: DiagnosticSeverity::Warning,
            message: format!("part {path} has a malformed content-transfer-encoding"),
            range: Some(body_range),
        });
    }

    let decoded_size = match &part.body {
        PartType::Text(t) | PartType::Html(t) => Some(t.len() as u64),
        PartType::Binary(b) | PartType::InlineBinary(b) => Some(b.len() as u64),
        PartType::Message(_) | PartType::Multipart(_) => None,
    };

    MimePart {
        path: path.clone(),
        content_type: content_type_of(part),
        charset: part
            .content_type()
            .and_then(|ct| ct.attribute("charset"))
            .map(str::to_owned),
        encoding: encoding_of(part),
        disposition,
        filename: part.attachment_name().map(str::to_owned),
        content_id: part
            .content_id()
            .map(|id| id.trim_matches(['<', '>']).to_owned()),
        header_range,
        body_range,
        decoded_size,
        is_attachment: disposition == Disposition::Attachment,
        children: (1..=child_indices.len())
            .filter_map(|n| u32::try_from(n).ok())
            .map(|n| path.child(n))
            .collect(),
    }
}

/// Resolves a part's content-transfer-encoding.
///
/// `mail-parser` only distinguishes the encodings it decodes; the header is
/// consulted for the rest so `7bit`, `8bit` and `binary` are reported faithfully.
fn encoding_of(part: &MessagePart<'_>) -> TransferEncoding {
    match part.encoding {
        Encoding::QuotedPrintable => TransferEncoding::QuotedPrintable,
        Encoding::Base64 => TransferEncoding::Base64,
        Encoding::None => part
            .headers
            .iter()
            .find(|h| {
                h.name
                    .as_str()
                    .eq_ignore_ascii_case("content-transfer-encoding")
            })
            .and_then(|h| h.value.as_text())
            .map_or(TransferEncoding::SevenBit, TransferEncoding::parse),
    }
}

fn content_type_of(part: &MessagePart<'_>) -> String {
    part.content_type().map_or_else(
        || {
            // RFC 2045 §5.2: absent Content-Type defaults to text/plain.
            String::from("text/plain")
        },
        |ct| {
            let main = ct.ctype().to_ascii_lowercase();
            ct.subtype().map_or_else(
                || main.clone(),
                |sub| format!("{main}/{}", sub.to_ascii_lowercase()),
            )
        },
    )
}

fn convert_headers(
    headers: &[mail_parser::Header<'_>],
    raw: &[u8],
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Vec<Header> {
    headers
        .iter()
        .map(|h| {
            let range = range_or_empty(h.offset_field, h.offset_end, raw, diagnostics);
            let raw_value = range_or_empty(h.offset_start, h.offset_end, raw, diagnostics)
                .slice(raw)
                .unwrap_or_default()
                .to_vec();
            // A header `mail-parser` gives no text value — `Received:`, and
            // anything it classifies as empty — would otherwise present a blank
            // value to every consumer, which blanked every `Received:` row in
            // the headers view. Nothing in those fields is RFC 2047-encoded, so
            // the unfolded raw bytes are the faithful decoded form.
            let decoded = match decode_header_value(&h.value) {
                text if text.is_empty() => unfold(&raw_value),
                text => text,
            };

            Header {
                name: h.name.as_str().to_owned(),
                decoded_value: decoded,
                raw_value,
                range,
            }
        })
        .collect()
}

/// Joins a folded header value into one line (RFC 5322 §2.2.3).
///
/// Folding whitespace is presentation, not content: a `Received:` field split
/// across four lines describes one hop. The bytes are lossily decoded because
/// this is the display form; `raw_value` remains the authority for exact octets.
fn unfold(raw_value: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw_value);
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        let line = if index == 0 {
            line.trim_end()
        } else {
            line.trim()
        };
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

fn decode_header_value(value: &mail_parser::HeaderValue<'_>) -> String {
    use mail_parser::HeaderValue;
    match value {
        HeaderValue::Text(t) => t.to_string(),
        HeaderValue::TextList(list) => list.join(", "),
        HeaderValue::Address(addr) => addr
            .iter()
            .map(|a| match (a.name(), a.address()) {
                (Some(name), Some(addr)) => format!("{name} <{addr}>"),
                (None, Some(addr)) => addr.to_owned(),
                (Some(name), None) => name.to_owned(),
                (None, None) => String::new(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        HeaderValue::DateTime(dt) => dt.to_rfc822(),
        HeaderValue::ContentType(ct) => ct.subtype().map_or_else(
            || ct.ctype().to_owned(),
            |sub| format!("{}/{sub}", ct.ctype()),
        ),
        // `Received:` carries a structured value that rendering back would
        // spell differently from the message; the caller falls back to the
        // unfolded raw bytes instead. See `convert_headers`.
        HeaderValue::Received(_) | HeaderValue::Empty => String::new(),
    }
}

/// Builds a byte range, degrading to an empty range rather than failing.
///
/// Offsets come from the parser and should always be ordered and in bounds;
/// treating a violation as a diagnostic rather than an error keeps the leniency
/// contract intact even if the parser surprises us.
fn range_or_empty(
    start: u32,
    end: u32,
    raw: &[u8],
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ByteRange {
    let (start, end) = (start as usize, end as usize);
    match ByteRange::new(start, end) {
        Some(range) if range.is_within(raw.len()) => range,
        _ => {
            diagnostics.push(ParseDiagnostic::warning(format!(
                "byte range [{start}..{end}) is invalid for a {} byte message",
                raw.len()
            )));
            ByteRange::empty_at(start.min(raw.len()))
        }
    }
}

fn collect_attachments(parts: &[MimePart]) -> Vec<Attachment> {
    parts
        .iter()
        .filter(|p| p.is_attachment || (p.content_id.is_some() && !p.is_multipart()))
        .map(|p| Attachment {
            id: AttachmentId::new(),
            part_path: p.path.clone(),
            filename: p.filename.clone(),
            content_type: p.content_type.clone(),
            content_id: p.content_id.clone(),
            size: p.decoded_size.unwrap_or(0),
            is_inline: !p.is_attachment && p.content_id.is_some(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;

    #[test]
    fn parses_a_simple_message() {
        let raw = b"From: a@test\r\nSubject: Hi\r\n\r\nBody text\r\n";
        let parsed = parse(raw);

        assert!(parsed.parse_ok);
        assert_eq!(
            parsed.header("subject").map(|h| h.decoded_value.as_str()),
            Some("Hi")
        );
        assert_eq!(parsed.text_parts.len(), 1);
    }

    #[test]
    fn part_bodies_are_exact_slices_of_raw() {
        let raw = b"Content-Type: text/plain\r\n\r\nexact body";
        let parsed = parse(raw);
        let Some(part) = parsed.root_part() else {
            panic!("expected a root part");
        };

        assert_eq!(part.body(raw), Some(&b"exact body"[..]));
    }

    #[test]
    fn multipart_children_get_imap_part_paths() {
        let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
plain\r\n\
--b\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>html</p>\r\n\
--b--\r\n";
        let parsed = parse(raw);
        let paths: Vec<String> = parsed.parts.iter().map(|p| p.path.to_string()).collect();

        assert!(paths.contains(&String::from("1")), "got {paths:?}");
        assert!(paths.contains(&String::from("2")), "got {paths:?}");
        assert_eq!(parsed.text_parts.len(), 1);
        assert_eq!(parsed.html_parts.len(), 1);
    }

    #[test]
    fn every_range_stays_within_the_raw_message() {
        let raw = b"Content-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\n\r\na\r\n--x--\r\n";
        let parsed = parse(raw);

        for part in &parsed.parts {
            assert!(part.body_range.is_within(raw.len()), "{:?}", part.path);
            assert!(part.header_range.is_within(raw.len()), "{:?}", part.path);
        }
        for header in &parsed.headers {
            assert!(header.range.is_within(raw.len()));
        }
    }

    #[test]
    fn malformed_input_never_panics() {
        // The leniency contract, exercised over hostile shapes.
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"\r\n".to_vec(),
            b"not a message at all".to_vec(),
            b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n".to_vec(),
            b"Subject: \xff\xfe invalid utf8\r\n\r\nbody".to_vec(),
            b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n".to_vec(),
            vec![0u8; 1024],
        ];
        for raw in cases {
            let parsed = parse(&raw);
            for part in &parsed.parts {
                assert!(part.body_range.is_within(raw.len()));
            }
        }
    }

    #[test]
    fn deeply_nested_multipart_is_bounded() {
        // Guards the stack against adversarial nesting.
        let mut raw = Vec::new();
        for i in 0..200 {
            raw.extend_from_slice(
                format!("Content-Type: multipart/mixed; boundary=b{i}\r\n\r\n--b{i}\r\n")
                    .as_bytes(),
            );
        }
        let parsed = parse(&raw);
        assert!(parsed.parts.len() <= MAX_DEPTH + 2, "depth must be bounded");
    }

    #[test]
    fn duplicate_headers_are_all_retained() {
        // Received: chains and DKIM ordering are semantically significant.
        let raw = b"Received: from a\r\nReceived: from b\r\nReceived: from c\r\n\r\nbody";
        let parsed = parse(raw);
        assert_eq!(parsed.headers_named("received").count(), 3);
    }

    #[test]
    fn absent_content_type_defaults_to_text_plain() {
        // RFC 2045 §5.2.
        let parsed = parse(b"Subject: x\r\n\r\nbody");
        assert_eq!(
            parsed.root_part().map(|p| p.content_type.as_str()),
            Some("text/plain")
        );
    }

    #[test]
    fn decoded_part_body_decodes_base64_and_quoted_printable() {
        let raw = b"Content-Type: multipart/alternative; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
Hello=20World=21\r\n\
--b\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
PHA+dGVzdDwvcD4=\r\n\
--b--\r\n";

        let Some(path1) = PartPath::parse("1") else {
            panic!("path 1 must parse");
        };
        let Some(path2) = PartPath::parse("2") else {
            panic!("path 2 must parse");
        };

        let Some(decoded1) = decoded_part_body(raw, &path1) else {
            panic!("part 1 must decode");
        };
        assert_eq!(String::from_utf8_lossy(&decoded1), "Hello World!");

        let Some(decoded2) = decoded_part_body(raw, &path2) else {
            panic!("part 2 must decode");
        };
        assert_eq!(String::from_utf8_lossy(&decoded2), "<p>test</p>");
    }

    #[test]
    fn a_received_header_has_a_decoded_value() {
        // Regression: `mail-parser` classifies `Received:` as a structured
        // value, and mapping that to the empty string blanked the value of
        // every hop in the headers view.
        let raw = b"Received: from a.example by b.example with ESMTP id X1;\r\n \
                    Tue, 1 Jul 2025 10:00:00 +0000\r\nSubject: hi\r\n\r\nbody";
        let parsed = parse(raw);

        let Some(received) = parsed.headers.iter().find(|h| h.name_matches("Received")) else {
            panic!("the Received header survives parsing");
        };

        assert!(
            !received.decoded_value.is_empty(),
            "a Received header must not decode to nothing"
        );
        assert!(received.decoded_value.contains("from a.example"));
        assert!(
            received.decoded_value.contains("Tue, 1 Jul 2025"),
            "the folded continuation joins the first line: {}",
            received.decoded_value
        );
        assert!(
            !received.decoded_value.contains('\n'),
            "an unfolded value is one line"
        );
    }

    #[test]
    fn unfolding_joins_continuations_with_a_single_space() {
        assert_eq!(unfold(b"one\r\n  two\r\n\tthree"), "one two three");
        assert_eq!(unfold(b"single"), "single");
        assert_eq!(unfold(b""), "");
    }
}
