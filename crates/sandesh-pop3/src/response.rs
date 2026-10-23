//! Wire formatting for POP3 responses (RFC 1939, RFC 2449).

/// A single-line positive response.
#[must_use]
pub fn ok(message: &str) -> String {
    if message.is_empty() {
        String::from("+OK\r\n")
    } else {
        format!("+OK {message}\r\n")
    }
}

/// A single-line negative response.
#[must_use]
pub fn err(message: &str) -> String {
    if message.is_empty() {
        String::from("-ERR\r\n")
    } else {
        format!("-ERR {message}\r\n")
    }
}

/// Dot-stuffs a multi-line byte payload according to RFC 1939 §3.
///
/// Any line starting with `.` has a second `.` prepended, and the output is
/// terminated with `\r\n.\r\n`.
///
/// Bare LF line endings are canonicalised to CRLF. Storage keeps the exact
/// octets received (`CORE_SPEC` §3) and SMTP preserves a bare LF verbatim, but a
/// multi-line POP3 response is framed as CRLF-terminated lines: sending an
/// interior bare LF raw would desynchronise the client's line parsing and, for
/// a line that happened to begin with '.', defeat the stuffing entirely. The
/// canonicalisation is therefore part of the wire encoding, not a mutation of
/// the stored message.
#[must_use]
pub fn dot_stuff(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 16);
    let mut at_line_start = true;
    let mut previous: Option<u8> = None;

    for &byte in raw {
        // A LF not already preceded by CR gets one, so every line ends CRLF.
        if byte == b'\n' && previous != Some(b'\r') {
            out.push(b'\r');
        }
        if at_line_start && byte == b'.' {
            out.push(b'.');
        }
        out.push(byte);

        at_line_start = byte == b'\n';
        previous = Some(byte);
    }

    // RFC 1939 §3 requires the termination `\r\n.\r\n`; the payload must end on
    // a complete line first. Every LF above already emitted CRLF, so the only
    // repair left is a payload that did not end with a newline at all.
    if !out.is_empty() && !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");

    out
}

/// Formats the result of a `TOP` command (RFC 1939 §7).
///
/// Returns the message headers plus up to `max_lines` of the body,
/// dot-stuffed and terminated with `\r\n.\r\n`.
#[must_use]
pub fn format_top(raw: &[u8], max_lines: usize) -> Vec<u8> {
    let mut split_pos = None;

    // Search for \r\n\r\n or \n\n
    for i in 0..raw.len() {
        if i + 3 < raw.len() && &raw[i..i + 4] == b"\r\n\r\n" {
            split_pos = Some((i + 2, i + 4));
            break;
        } else if i + 1 < raw.len() && &raw[i..i + 2] == b"\n\n" {
            split_pos = Some((i + 1, i + 2));
            break;
        }
    }

    let (headers, body) = match split_pos {
        Some((h_end, b_start)) => (&raw[..h_end], &raw[b_start..]),
        None => (raw, &[][..]),
    };

    let mut top_bytes = Vec::new();
    // Headers always end with CRLF
    top_bytes.extend_from_slice(headers);
    if !top_bytes.ends_with(b"\r\n") {
        if top_bytes.ends_with(b"\n") {
            top_bytes.pop();
        }
        top_bytes.extend_from_slice(b"\r\n");
    }
    // Blank line separating headers and body
    top_bytes.extend_from_slice(b"\r\n");

    // Take up to max_lines lines from body
    let mut line_count = 0;
    let mut start = 0;

    while start < body.len() && line_count < max_lines {
        let mut end = start;
        while end < body.len() && body[end] != b'\n' {
            end += 1;
        }
        if end < body.len() && body[end] == b'\n' {
            end += 1;
        }
        top_bytes.extend_from_slice(&body[start..end]);
        line_count += 1;
        start = end;
    }

    dot_stuff(&top_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_responses() {
        assert_eq!(ok(""), "+OK\r\n");
        assert_eq!(ok("ready"), "+OK ready\r\n");
        assert_eq!(err(""), "-ERR\r\n");
        assert_eq!(err("permission denied"), "-ERR permission denied\r\n");
    }

    #[test]
    fn dot_stuffing_basic() {
        let input = b"From: a@b.c\r\nSubject: test\r\n\r\nHello world\r\n";
        let stuffed = dot_stuff(input);
        assert_eq!(
            stuffed,
            b"From: a@b.c\r\nSubject: test\r\n\r\nHello world\r\n.\r\n"
        );
    }

    #[test]
    fn dot_stuffing_leading_dots() {
        let input = b".starts with dot\r\n..two dots\r\nnormal\r\n.another dot\r\n";
        let stuffed = dot_stuff(input);
        assert_eq!(
            stuffed,
            b"..starts with dot\r\n...two dots\r\nnormal\r\n..another dot\r\n.\r\n"
        );
    }

    #[test]
    fn format_top_basic() {
        let input = b"From: a@b.c\r\nSubject: test\r\n\r\nLine 1\r\nLine 2\r\nLine 3\r\n";
        let top0 = format_top(input, 0);
        assert_eq!(top0, b"From: a@b.c\r\nSubject: test\r\n\r\n.\r\n");

        let top1 = format_top(input, 1);
        assert_eq!(top1, b"From: a@b.c\r\nSubject: test\r\n\r\nLine 1\r\n.\r\n");

        let top2 = format_top(input, 2);
        assert_eq!(
            top2,
            b"From: a@b.c\r\nSubject: test\r\n\r\nLine 1\r\nLine 2\r\n.\r\n"
        );
    }
}
