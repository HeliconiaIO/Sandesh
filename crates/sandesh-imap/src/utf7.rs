//! Modified UTF-7 for mailbox names (RFC 3501 §5.1.3).
//!
//! `IMAP4rev2` carries mailbox names as UTF-8 and the domain stores them as
//! Unicode ([`sandesh_core::MailboxName`]), so this is a *wire* encoding
//! applied at the adapter boundary and nowhere else.
//!
//! # Why this exists at all in a rev2 server
//!
//! ADR-042 advertises the `IMAP4rev1` capability name so pre-RFC-9051 clients
//! will talk to us. Those clients encode non-ASCII mailbox names in modified
//! UTF-7. Without a decoder the encoded form falls through as a *literal*
//! name, so `CREATE "Wichtig-&APw-ber"` from a rev1 client and
//! `CREATE "Wichtig-über"` from a rev2 client produce **two different
//! mailboxes for the same logical folder** — found against a live server, not
//! by review. Decoding on the way in collapses them onto one name.
//!
//! # The encoding, and how it differs from RFC 2152 UTF-7
//!
//! - Printable ASCII (0x20–0x7E) is literal, except `&`, which is `&-`.
//! - Anything else is UTF-16BE, base64'd, wrapped in `&` … `-`.
//! - The base64 alphabet substitutes `,` for `/`, because `/` is the
//!   hierarchy delimiter.
//! - Base64 padding (`=`) is omitted.
//!
//! # Decoding is deliberately lenient in exactly one respect
//!
//! A name that is not valid modified UTF-7 is returned **unchanged** rather
//! than rejected. A rev2 client is entitled to send a literal `&` in a UTF-8
//! name, and there is no way to tell that apart from a malformed escape; the
//! RFC only obliges rev1 clients to encode. Returning the input preserves the
//! rev2 name exactly, and a rev1 client that sends a genuinely broken escape
//! gets a mailbox named what it literally typed — a visible, harmless outcome,
//! where rejecting would break a legal rev2 name.

/// The base64 alphabet, with `,` in place of `/` (RFC 3501 §5.1.3).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// Decodes a modified-UTF-7 mailbox name to Unicode.
///
/// Returns the input unchanged when it is not valid modified UTF-7; see the
/// module docs for why that is a decode rule and not a bug.
#[must_use]
pub fn decode(name: &str) -> String {
    if !name.contains('&') {
        return String::from(name);
    }
    try_decode(name).unwrap_or_else(|| String::from(name))
}

/// The fallible core of [`decode`]. `None` means "not valid modified UTF-7".
fn try_decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Only printable ASCII is legal unencoded; anything else means the
            // input was never modified UTF-7 (most likely it is already UTF-8).
            if !(0x20..=0x7E).contains(&bytes[i]) {
                return None;
            }
            out.push(char::from(bytes[i]));
            i += 1;
            continue;
        }

        // `&-` is a literal ampersand.
        let end = bytes[i + 1..].iter().position(|b| *b == b'-')? + i + 1;
        if end == i + 1 {
            out.push('&');
            i = end + 1;
            continue;
        }

        out.push_str(&decode_run(&bytes[i + 1..end])?);
        i = end + 1;
    }

    Some(out)
}

/// Decodes one `&`…`-` run: base64 → UTF-16BE code units → text.
fn decode_run(run: &[u8]) -> Option<String> {
    // Unpack base64 into a bit buffer, six bits at a time.
    let mut units = Vec::with_capacity(run.len() / 2);
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;

    for byte in run {
        // The index is bounded by the 64-entry alphabet, so this conversion
        // cannot fail; `try_from` states that rather than asserting it in a
        // comment beside a cast.
        let position = ALPHABET.iter().position(|c| c == byte)?;
        let value = u32::try_from(position).ok()?;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 16 {
            bits -= 16;
            // Masked to 16 bits, so the truncation is the intended extraction
            // of one UTF-16 code unit.
            units.push(u16::try_from((buffer >> bits) & 0xFFFF).ok()?);
        }
    }

    // Whatever is left must be zero padding, and fewer than six bits of it.
    // A non-zero remainder is a malformed run, not a decodable one.
    if bits >= 6 || (buffer & ((1 << bits) - 1)) != 0 {
        return None;
    }

    // An empty run (`&&-`) encodes nothing; RFC 3501 has no such form.
    if units.is_empty() {
        return None;
    }

    // Rejects unpaired surrogates, which is what makes the output valid UTF-8.
    String::from_utf16(&units).ok()
}

/// Encodes a Unicode mailbox name as modified UTF-7.
///
/// The inverse of [`decode`] for every name this server can hold: `decode`
/// composed with `encode` is the identity on any validated `MailboxName`.
#[must_use]
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();

    for c in name.chars() {
        if ('\u{20}'..='\u{7E}').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0_u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Emits the buffered code units as one `&`…`-` base64 run.
fn flush(pending: &mut Vec<u16>, out: &mut String) {
    if pending.is_empty() {
        return;
    }

    out.push('&');
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    for unit in pending.iter() {
        buffer = (buffer << 16) | u32::from(*unit);
        bits += 16;
        while bits >= 6 {
            bits -= 6;
            out.push(char::from(ALPHABET[((buffer >> bits) & 0x3F) as usize]));
        }
    }
    if bits > 0 {
        // Left-align the remaining bits into a final six-bit group.
        out.push(char::from(
            ALPHABET[((buffer << (6 - bits)) & 0x3F) as usize],
        ));
    }
    out.push('-');
    pending.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_unchanged_in_both_directions() {
        for name in ["INBOX", "Work/Projects", "Sent Items", "a.b-c_d"] {
            assert_eq!(encode(name), name, "encode {name}");
            assert_eq!(decode(name), name, "decode {name}");
        }
    }

    #[test]
    fn the_rfc_3501_example_round_trips() {
        // RFC 3501 §5.1.3's own example.
        let unicode = "~peter/mail/台北/日本語";
        let wire = "~peter/mail/&U,BTFw-/&ZeVnLIqe-";
        assert_eq!(encode(unicode), wire);
        assert_eq!(decode(wire), unicode);
    }

    #[test]
    fn the_live_duplicate_folder_case() {
        // The exact pair that produced two mailboxes against a running server.
        assert_eq!(decode("Wichtig-&APw-ber"), "Wichtig-über");
        assert_eq!(encode("Wichtig-über"), "Wichtig-&APw-ber");
    }

    #[test]
    fn a_literal_ampersand_is_escaped() {
        assert_eq!(encode("Ben & Jerry"), "Ben &- Jerry");
        assert_eq!(decode("Ben &- Jerry"), "Ben & Jerry");
        assert_eq!(decode(&encode("R&D")), "R&D");
    }

    #[test]
    fn a_non_bmp_character_uses_a_surrogate_pair() {
        // U+1F4E7 is outside the BMP, so UTF-16 needs two code units and the
        // decoder must pair them rather than reject them.
        let mail = "Mail \u{1F4E7}";
        assert_eq!(decode(&encode(mail)), mail);
    }

    #[test]
    fn malformed_input_is_returned_unchanged() {
        for broken in [
            "no-terminator&APw", // run never closed
            "&!!!-",             // not base64
            "&&-",               // empty run
            "already-über",      // raw UTF-8, not encoded
        ] {
            assert_eq!(decode(broken), broken, "{broken}");
        }
    }

    #[test]
    fn an_unpaired_surrogate_is_refused() {
        // D800 alone is not valid UTF-16, so the run cannot decode and the
        // input survives verbatim rather than becoming a replacement char.
        assert_eq!(decode("&2AA-"), "&2AA-");
    }

    #[test]
    fn decode_undoes_encode_for_a_range_of_scripts() {
        for name in [
            "Ärchiv",
            "Wichtig-über",
            "受信箱",
            "Входящие",
            "Παραλαβή",
            "Work/Ärchiv/2024",
            "mixed & ünicode",
        ] {
            assert_eq!(decode(&encode(name)), name, "{name}");
        }
    }

    #[test]
    fn encoding_never_emits_the_hierarchy_delimiter_inside_a_run() {
        // The `,`-for-`/` substitution exists so an encoded run cannot be
        // mistaken for a hierarchy separator. U+00FF encodes to a run that
        // would contain `/` under standard base64.
        let encoded = encode("\u{4E2D}\u{6587}");
        let run = encoded.trim_start_matches('&').trim_end_matches('-');
        assert!(!run.contains('/'), "{encoded}");
    }

    #[test]
    fn decoding_never_panics() {
        // Adapter-boundary input is attacker-controlled.
        for seed in 0_u32..2000 {
            let bytes: Vec<u8> = (0..(seed % 11))
                .map(|i| {
                    let candidates = b"&-,+ABZaz09/\"\\ \x7f";
                    candidates[((seed as usize) + i as usize) % candidates.len()]
                })
                .collect();
            if let Ok(text) = core::str::from_utf8(&bytes) {
                let _ = decode(text);
            }
        }
    }
}
