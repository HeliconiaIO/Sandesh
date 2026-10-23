//! `mboxrd` framing — the container format for bulk export and import.
//!
//! # Why `mboxrd` and not `mboxo`
//!
//! mbox has no length prefix. A message ends where the next one begins, and
//! "the next one begins" is detected by a line starting `From `. A message
//! whose own body contains such a line therefore has to be escaped, and the
//! mbox variants differ only in how they do it:
//!
//! - **`mboxo`** prefixes `>` to lines matching `From ` and *never removes it*
//!   on read. It is lossy by construction: export-then-import turns `From the
//!   desk of...` into `>From the desk of...`, permanently. Each round trip adds
//!   another `>`.
//! - **`mboxrd`** prefixes `>` to lines matching `>*From ` — that is, to a bare
//!   `From ` *and* to any already-quoted form — and strips exactly one `>` from
//!   such lines on read. The transform is invertible, so the round trip is
//!   byte-identical.
//!
//! Phase 8's acceptance criterion is that "exported messages re-import
//! byte-identically". Only `mboxrd` can meet it, which is why §2 of the phase
//! plan named it specifically and why [`quote_from_lines`] quotes the
//! already-quoted forms rather than only the bare one. The asymmetric variant
//! is the bug this module exists to not have.
//!
//! # Line endings
//!
//! Stored messages preserve their line endings verbatim, bare LF included — a
//! Phase 2 guarantee with its own acceptance criterion. mbox is a line-oriented
//! format, so framing has to find line boundaries without *normalising* them.
//! Everything here splits on LF and rejoins with the original bytes, so a bare
//! LF stays a bare LF and a CRLF stays a CRLF across both directions.

use crate::Timestamp;

/// The separator that opens every mbox message.
const FROM_: &[u8] = b"From ";

/// The pseudo-address used in the `From ` separator line.
///
/// The separator's address field is not the envelope sender and no reader
/// should treat it as one: it is part of the framing, not the message. Real
/// mbox writers vary wildly here. A fixed sentinel keeps export deterministic,
/// which matters for a test that compares exported bytes.
const SEPARATOR_ADDRESS: &str = "sandesh@localhost";

/// Quotes `From `-lines for writing into an mbox, per the `mboxrd` rule.
///
/// A line consisting of zero or more `>` followed by `From ` gains one more
/// `>`. Every other line is untouched. [`unquote_from_lines`] is the exact
/// inverse.
#[must_use]
pub fn quote_from_lines(body: &[u8]) -> Vec<u8> {
    transform_lines(body, |line, out| {
        if is_quoted_from(line) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    })
}

/// Removes one level of `From `-line quoting, per the `mboxrd` rule.
///
/// The inverse of [`quote_from_lines`]: a line of one or more `>` followed by
/// `From ` loses exactly one `>`. A bare `From ` line — which a conforming
/// writer never produces inside a body — is left alone rather than corrupted.
#[must_use]
pub fn unquote_from_lines(body: &[u8]) -> Vec<u8> {
    transform_lines(body, |line, out| {
        if line.first() == Some(&b'>') && is_quoted_from(&line[1..]) {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    })
}

/// Is `line` zero or more `>` followed by `From `?
fn is_quoted_from(line: &[u8]) -> bool {
    let rest = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
    line[rest..].starts_with(FROM_)
}

/// Applies `f` to each LF-delimited line, preserving the original terminators.
///
/// The line handed to `f` excludes its terminator (and any CR before it), so a
/// rule can match on content; the terminator is re-emitted verbatim afterwards,
/// which is what keeps a bare LF bare and a CRLF intact.
fn transform_lines(body: &[u8], mut f: impl FnMut(&[u8], &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + body.len() / 64);
    let mut rest = body;

    loop {
        let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
            // A trailing fragment with no terminator.
            f(rest, &mut out);
            break;
        };

        let with_cr = &rest[..nl];
        let content = with_cr.strip_suffix(b"\r").unwrap_or(with_cr);
        f(content, &mut out);
        out.extend_from_slice(&rest[content.len()..=nl]);
        rest = &rest[nl + 1..];
    }

    out
}

/// Renders one message as an mbox entry, separator line included.
///
/// The entry is self-delimiting when concatenated with others: it opens with a
/// `From ` separator and ends with a blank line, which is what lets
/// [`parse`] find the boundaries again.
#[must_use]
pub fn write_entry(raw: &[u8], received_at: Timestamp) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 128);

    out.extend_from_slice(FROM_);
    out.extend_from_slice(SEPARATOR_ADDRESS.as_bytes());
    out.push(b' ');
    out.extend_from_slice(asctime(received_at).as_bytes());
    out.push(b'\n');

    // A message that does not end at a line boundary cannot be framed as-is:
    // mbox needs the next `From ` separator to start a line, so a terminator
    // has to be added — and an added terminator is a byte the message did not
    // have, which breaks the round trip. `testdata/malformed/` holds four such
    // fixtures (a truncated message is exactly this shape), so this is a real
    // case and not a hypothetical one.
    //
    // The fix is to record that the terminator was synthetic. `Content-Length`
    // — the same header mboxcl2 uses for the same problem — states the true
    // octet count, and `parse` truncates back to it. A reader that ignores the
    // header still sees a well-formed mbox, one byte long in the message.
    let needs_terminator = !raw.is_empty() && !raw.ends_with(b"\n");
    if needs_terminator || raw.is_empty() {
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(raw.len().to_string().as_bytes());
        out.push(b'\n');
    }

    out.extend_from_slice(&quote_from_lines(raw));

    // Each entry is terminated by a blank line, so the next `From ` separator
    // starts a line. A message already ending in a newline needs only the blank
    // line; one that does not needs a terminator first.
    if needs_terminator {
        out.push(b'\n');
    }
    out.push(b'\n');

    out
}

/// Renders a whole mbox from messages and their receipt times.
#[must_use]
pub fn write<'a>(messages: impl IntoIterator<Item = (&'a [u8], Timestamp)>) -> Vec<u8> {
    let mut out = Vec::new();
    for (raw, received_at) in messages {
        out.extend_from_slice(&write_entry(raw, received_at));
    }
    out
}

/// Splits an mbox into its messages, undoing `From `-quoting.
///
/// Boundaries are `From ` at the start of a line. The bytes before the first
/// separator are discarded: some writers emit a leading comment, and a file
/// that opens with something else is more likely to be a stray `.eml` than a
/// corrupt mbox — [`parse`] is not the place to adjudicate that, so it yields
/// nothing rather than guessing.
///
/// Returns each message's raw bytes, with the trailing blank line that
/// [`write_entry`] added removed, so `parse(write(m)) == m`.
#[must_use]
pub fn parse(data: &[u8]) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    let mut exact_len: Option<usize> = None;
    let mut just_opened = false;
    let mut rest = data;

    loop {
        let (line_with_term, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(nl) => (&rest[..=nl], &rest[nl + 1..]),
            None if rest.is_empty() => break,
            None => (rest, &rest[rest.len()..]),
        };

        let line = trim_terminator(line_with_term);

        if line.starts_with(FROM_) {
            // A separator: close the entry in progress and open a new one.
            if let Some(body) = current.take() {
                messages.push(finish_entry(&body, exact_len.take()));
            }
            current = Some(Vec::new());
            just_opened = true;
        } else if let Some(body) = current.as_mut() {
            // A `Content-Length` directly after the separator is *framing*, not
            // a message header: it records the true octet count for a message
            // that did not end at a line boundary (see `write_entry`). Only the
            // line immediately after the separator is treated this way, so a
            // message legitimately carrying its own `Content-Length` header is
            // untouched.
            if just_opened && let Some(value) = line.strip_prefix(b"Content-Length: ") {
                exact_len = std::str::from_utf8(value).ok().and_then(|v| v.parse().ok());
            } else {
                body.extend_from_slice(line_with_term);
            }
            just_opened = false;
        }

        if next.is_empty() {
            break;
        }
        rest = next;
    }

    if let Some(body) = current {
        messages.push(finish_entry(&body, exact_len));
    }

    messages
}

/// Strips a trailing LF and any CR before it.
fn trim_terminator(line: &[u8]) -> &[u8] {
    let no_lf = line.strip_suffix(b"\n").unwrap_or(line);
    no_lf.strip_suffix(b"\r").unwrap_or(no_lf)
}

/// Undoes quoting and removes the blank line `write_entry` appended.
fn finish_entry(body: &[u8], exact_len: Option<usize>) -> Vec<u8> {
    let mut unquoted = unquote_from_lines(body);

    // An exact length from the framing is authoritative: it is the count the
    // writer recorded precisely because the terminators below cannot be
    // distinguished from the message's own. Truncating to it restores a
    // message that did not end at a line boundary, and an empty one.
    if let Some(len) = exact_len {
        if len <= unquoted.len() {
            unquoted.truncate(len);
        }
        return unquoted;
    }

    // Remove exactly the one blank line written as a terminator, preserving
    // whatever the message itself ended with. Checked longest-first so a CRLF
    // message does not lose a CR.
    for terminator in [b"\r\n".as_slice(), b"\n".as_slice()] {
        if let Some(trimmed) = unquoted.strip_suffix(terminator) {
            return trimmed.to_vec();
        }
    }

    unquoted
}

/// Formats a timestamp as the `asctime` form the separator line uses.
///
/// `Day Mon DD HH:MM:SS YYYY`, as `ctime(3)` produces. Deliberately hand-rolled
/// rather than pulled from a formatting crate: this is framing metadata no
/// reader parses, and `sandesh-core` takes no dependency it does not need.
fn asctime(ts: Timestamp) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let secs = ts.as_micros().div_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday. `rem_euclid` is non-negative, so this indexes
    // in range for a negative `days` (a pre-epoch timestamp) too.
    let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);

    // Both lookups go through `get`: `civil_from_days` returns a month in 1..=12
    // and this is the only caller, but an out-of-range index here would panic
    // while *writing an export*, which is a poor place to discover an
    // arithmetic slip. A wrong label is recoverable; a panic mid-corpus is not.
    let weekday_name = DAYS.get(weekday).copied().unwrap_or("Thu");
    let month_name = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTHS.get(index).copied())
        .unwrap_or("Jan");

    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        weekday_name,
        month_name,
        day,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        year,
    )
}

/// Converts days since the Unix epoch to a civil `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, which is exact over the whole range and
/// needs no table. Month is 1..=12 and day 1..=31 by construction.
///
/// Everything stays `i64`. The month and day are provably small enough for a
/// `u32`, but narrowing them would buy nothing — the only consumer formats them
/// — while costing a cast that has to be either checked or excused.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };

    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Timestamp {
        Timestamp::from_micros(1_700_000_000_000_000)
    }

    #[test]
    fn quoting_is_invertible() {
        // The `mboxrd` property: unquote(quote(x)) == x for any body. The
        // already-quoted forms are the cases `mboxo` gets wrong.
        for body in [
            b"plain body\n".as_slice(),
            b"From the desk of Alice\n",
            b">From already quoted\n",
            b">>From doubly quoted\n",
            b"before\nFrom here\nafter\n",
            b"From ",
            b"no trailing newline",
            b"",
        ] {
            let quoted = quote_from_lines(body);
            assert_eq!(
                unquote_from_lines(&quoted),
                body,
                "round trip failed for {body:?}",
            );
        }
    }

    #[test]
    fn quoting_adds_exactly_one_level() {
        assert_eq!(quote_from_lines(b"From x\n"), b">From x\n");
        assert_eq!(quote_from_lines(b">From x\n"), b">>From x\n");
        // Not a `From ` line: "Fromage" has no space, "> From" has a space
        // before `From`. Neither may be touched.
        assert_eq!(quote_from_lines(b"Fromage\n"), b"Fromage\n");
        assert_eq!(quote_from_lines(b"> From x\n"), b"> From x\n");
    }

    #[test]
    fn mboxo_asymmetry_would_fail_this() {
        // The specific defect `mboxrd` exists to avoid: a body line that is
        // already `>From ` must come back as `>From `, not `From ` (which
        // unquoting a non-quoting writer's output would give) and not
        // `>>From ` (which quoting without unquoting would give).
        let body = b">From the desk of Alice\n";
        let entry = write_entry(body, ts());
        let parsed = parse(&entry);
        assert_eq!(parsed, vec![body.to_vec()]);
    }

    #[test]
    fn line_endings_survive_the_round_trip() {
        // Phase 2 guarantees bare LF stays bare LF. Framing must not normalise.
        for body in [
            b"Subject: crlf\r\n\r\nbody\r\n".as_slice(),
            b"Subject: bare lf\n\nbody\n",
            b"Subject: mixed\r\n\nboth\r\nkinds\n",
        ] {
            let entry = write_entry(body, ts());
            assert_eq!(parse(&entry), vec![body.to_vec()]);
        }
    }

    #[test]
    fn multiple_messages_round_trip() {
        let a = b"Subject: one\r\n\r\nFrom the start\r\n".as_slice();
        let b = b"Subject: two\r\n\r\nbody\r\n".as_slice();
        let c = b"Subject: three\r\n\r\n>From quoted\r\n".as_slice();

        let mbox = write([(a, ts()), (b, ts()), (c, ts())]);
        assert_eq!(parse(&mbox), vec![a.to_vec(), b.to_vec(), c.to_vec()]);
    }

    #[test]
    fn separator_opens_each_entry() {
        let mbox = write([(b"Subject: x\r\n\r\nbody\r\n".as_slice(), ts())]);
        assert!(mbox.starts_with(b"From sandesh@localhost "));
    }

    #[test]
    fn leading_junk_before_the_first_separator_is_discarded() {
        let mut data = b"# a comment some writers emit\n".to_vec();
        data.extend_from_slice(&write_entry(b"Subject: x\r\n\r\nbody\r\n", ts()));
        assert_eq!(parse(&data), vec![b"Subject: x\r\n\r\nbody\r\n".to_vec()]);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse(b"").is_empty());
        assert!(parse(b"not an mbox at all\n").is_empty());
    }

    #[test]
    fn messages_not_ending_at_a_line_boundary_round_trip() {
        // The case mbox framing cannot represent without help: the terminator
        // that makes the next separator start a line is a byte the message
        // never had. `testdata/malformed/` holds four fixtures of this shape.
        for body in [
            b"Subject: truncated mid-header".as_slice(),
            b"Subject: x\r\n\r\nbody with no final newline",
            b"a",
            b"From ",
        ] {
            let entry = write_entry(body, ts());
            assert_eq!(
                parse(&entry),
                vec![body.to_vec()],
                "unterminated message did not survive: {body:?}",
            );
        }
    }

    #[test]
    fn an_empty_message_round_trips() {
        // `testdata/malformed/empty.eml` is zero bytes. Phase 2's rule is that
        // malformed mail is stored, not rejected, so the framing has to carry
        // it rather than silently dropping one message from a corpus.
        let entry = write_entry(b"", ts());
        assert_eq!(parse(&entry), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn a_messages_own_content_length_header_is_not_framing() {
        // Only the line directly after the separator is framing. A message
        // whose *own* headers include Content-Length must keep it.
        let body = b"Subject: x\r\nContent-Length: 999\r\n\r\nbody\r\n".as_slice();
        let entry = write_entry(body, ts());
        assert_eq!(parse(&entry), vec![body.to_vec()]);
    }

    #[test]
    fn mixed_terminated_and_unterminated_messages_round_trip() {
        // The framing must stay self-describing per entry: an exact length on
        // one message may not leak into the next.
        let a = b"Subject: terminated\r\n\r\nbody\r\n".as_slice();
        let b = b"Subject: unterminated\r\n\r\nbody".as_slice();
        let c = b"".as_slice();
        let d = b"Subject: also terminated\r\n\r\nbody\r\n".as_slice();

        let mbox = write([(a, ts()), (b, ts()), (c, ts()), (d, ts())]);
        assert_eq!(
            parse(&mbox),
            vec![a.to_vec(), b.to_vec(), c.to_vec(), d.to_vec()],
        );
    }

    #[test]
    fn asctime_matches_ctime() {
        // 1700000000 == Tue Nov 14 22:13:20 2023 UTC.
        assert_eq!(asctime(ts()), "Tue Nov 14 22:13:20 2023");
        // The epoch itself, and a leap day.
        assert_eq!(
            asctime(Timestamp::from_micros(0)),
            "Thu Jan  1 00:00:00 1970"
        );
        assert_eq!(
            asctime(Timestamp::from_micros(1_709_164_800_000_000)),
            "Thu Feb 29 00:00:00 2024",
        );
    }

    #[test]
    fn a_pre_epoch_timestamp_formats_rather_than_panicking() {
        // A negative timestamp is the input that makes the weekday and month
        // arithmetic go negative. Nothing in Sandesh produces one, which is
        // exactly why it is worth pinning: an export must not panic on a
        // `received_at` that some future path sets to a pre-1970 date.
        assert_eq!(
            asctime(Timestamp::from_micros(-1_000_000)),
            "Wed Dec 31 23:59:59 1969",
        );
        assert_eq!(
            asctime(Timestamp::from_micros(-86_400_000_000)),
            "Wed Dec 31 00:00:00 1969",
        );
    }
}
