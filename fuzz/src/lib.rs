//! The body of every fuzz target, as ordinary library code.
//!
//! Each `check_*` function takes one input and asserts the properties
//! `TESTING_STRATEGY.md` §9 names for that parser. The `fuzz_targets/` binaries
//! are thin `fuzz_target!` wrappers around these.
//!
//! Keeping the logic here rather than inside the harness macro buys two things:
//! the assertions are typechecked by a stable toolchain (the fuzz binaries need
//! nightly, so nothing else in CI would ever compile them), and the same checks
//! can be replayed over a fixed corpus — see `tests/corpus.rs`, which runs every
//! `testdata/` fixture and every crash artifact through them on stable.

use sandesh_core::{ByteRange, SearchQuery, parse_date_text, parse_filter_query};
use sandesh_imap::{MAX_NESTING_DEPTH, MAX_TOKENS, Parsed, parse_line};
use sandesh_pop3::Pop3Command;
use sandesh_smtp::parser::{parse as parse_smtp, parse_path};

/// A command line is bounded by the framing layer long before it reaches a
/// parser, so inputs past this length test nothing the parser owns.
const MAX_LINE: usize = 4096;

/// What an IMAP session allows. The bound is the point: the parser must refuse
/// a larger literal rather than allocate for it.
const MAX_LITERAL: usize = 64 * 1024;

/// Generous next to anything the filter grammar can legitimately produce, and
/// far below what would overflow a stack.
const MAX_PLAN_DEPTH: usize = 64;

/// MIME parser: never panics; never loses bytes; terminates.
///
/// "Never loses bytes" is the property worth real effort. `sandesh_core::parse`
/// describes a message by *byte range* over the input — the MIME-tree ↔ raw
/// linkage the Web UI draws depends on it — so every range it reports must
/// index the bytes it was handed. An out-of-range span would not fail here but
/// later, in a slice, far from the input that produced it.
pub fn check_mime(data: &[u8]) {
    let parsed = sandesh_core::parse(data);

    for header in &parsed.headers {
        within(header.range, data, "header");
    }
    for part in &parsed.parts {
        within(part.header_range, data, "part header");
        within(part.body_range, data, "part body");
    }

    // Every projected path must name a part that exists. A dangling path is how
    // a "part not found" unwrap reaches production.
    for path in parsed.text_parts.iter().chain(&parsed.html_parts) {
        assert!(
            parsed.parts.iter().any(|part| part.path == *path),
            "body path {path:?} names no part"
        );
    }
    for attachment in &parsed.attachments {
        assert!(
            parsed
                .parts
                .iter()
                .any(|part| part.path == attachment.part_path),
            "attachment path {:?} names no part",
            attachment.part_path
        );
    }

    // Parsing is pure: the same bytes must give the same answer.
    let again = sandesh_core::parse(data);
    assert_eq!(
        parsed.parse_ok, again.parse_ok,
        "parse is not deterministic for this input"
    );
    assert_eq!(
        parsed.parts.len(),
        again.parts.len(),
        "parse is not deterministic for this input"
    );
}

fn within(range: ByteRange, data: &[u8], what: &str) {
    assert!(
        range.is_within(data.len()),
        "{what} range {}..{} runs past the {}-byte input",
        range.start(),
        range.end(),
        data.len()
    );
    // The accessor re-checks bounds rather than trusting the range, so this
    // also proves the two agree.
    assert!(
        range.slice(data).is_some(),
        "{what} range {}..{} does not slice its own input",
        range.start(),
        range.end()
    );
}

/// SMTP command parser: never panics; bounded memory.
///
/// Both halves of the grammar are exercised: the verb parser, and `parse_path`,
/// which handles the `<...>` reverse- and forward-paths and sees the most
/// hostile input in practice.
pub fn check_smtp(data: &[u8]) {
    if data.len() > MAX_LINE {
        return;
    }

    // The framing layer strips CRLF before a line reaches the parser.
    let line: Vec<u8> = data
        .iter()
        .copied()
        .filter(|byte| *byte != b'\r' && *byte != b'\n')
        .collect();

    let first = parse_smtp(&line);
    assert_eq!(
        first.is_ok(),
        parse_smtp(&line).is_ok(),
        "SMTP parse is not deterministic for this input"
    );

    // `parse_path` takes `&str`, which is how the session layer reaches it.
    if let Ok(text) = core::str::from_utf8(&line) {
        let _ = parse_path(text);
    }
}

/// IMAP command parser: never panics; bounded memory.
///
/// `parse_line` is only half the path. A command carrying a literal stops
/// mid-parse and returns a `PendingLiteral` the session resumes once it has read
/// the octets, so stopping at `parse_line` would never reach `resume` or the
/// nesting and token bounds that guard it.
pub fn check_imap(data: &[u8]) {
    if data.len() > 2 * MAX_LINE {
        return;
    }

    // A bare CR or LF inside a command line is the framing layer's business.
    let line: Vec<u8> = data
        .iter()
        .copied()
        .filter(|byte| *byte != b'\r' && *byte != b'\n')
        .collect();

    // The bounds the parser advertises must be real; a build that set either to
    // zero would accept nothing and pass every other assertion vacuously. Both
    // are constants, so this is checked once at compile time rather than on
    // every iteration.
    const { assert!(MAX_NESTING_DEPTH > 0 && MAX_TOKENS > 0) };

    let first = parse_line(&line, MAX_LITERAL);
    assert_eq!(
        first.is_ok(),
        parse_line(&line, MAX_LITERAL).is_ok(),
        "parse_line is not deterministic for this input"
    );

    if let Ok(Parsed::NeedLiteral(pending)) = first {
        // A pending literal must never have been allowed to declare more than
        // the cap: the bound exists to be checked *before* allocating.
        assert!(
            pending.length() <= MAX_LITERAL,
            "pending literal of {} octets exceeds the {MAX_LITERAL}-octet cap",
            pending.length()
        );

        // Resume with a payload of exactly the declared length, as the session
        // does, using the line itself as the trailing remainder.
        let payload = vec![b'x'; pending.length()];
        let _ = pending.resume(payload, &line, MAX_LITERAL);
    }

    // A zero cap must reject every non-empty literal without allocating for one.
    if let Ok(Parsed::NeedLiteral(pending)) = parse_line(&line, 0) {
        assert_eq!(
            pending.length(),
            0,
            "a zero literal cap admitted a non-empty literal"
        );
    }
}

/// POP3 command parser: never panics.
///
/// `crates/sandesh-pop3/tests/parser_robustness.rs` already asserts this over
/// 40,000 generated cases. That test is a floor, not a replacement: it explores
/// only the shapes its generator was written to produce.
pub fn check_pop3(data: &[u8]) {
    if data.len() > MAX_LINE {
        return;
    }

    // The session layer decodes the line before parsing, so non-UTF-8 never
    // reaches `parse`.
    let Ok(line) = core::str::from_utf8(data) else {
        return;
    };

    // A command that parses must survive the trip again: the parser trims its
    // own line endings, so an already-trimmed line is the same command.
    if let Ok(command) = Pop3Command::parse(line) {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        assert_eq!(
            Pop3Command::parse(trimmed).as_ref(),
            Ok(&command),
            "parse is not stable under the trimming it performs itself"
        );
    }
}

/// Search query parser: never panics; terminates.
///
/// `parse_filter_query` is infallible by design — every input is *some* plan,
/// because the UI's filter box must never refuse to search. That makes the
/// interesting property structural: `SearchQuery` is a recursive tree, and a
/// plan nested without bound would fail not here but in the evaluator's
/// recursion, or in the generated `Drop`, as a stack overflow no `Result` can
/// express.
pub fn check_search(data: &[u8]) {
    if data.len() > MAX_LINE {
        return;
    }
    let Ok(input) = core::str::from_utf8(data) else {
        return;
    };

    let depth = depth_of(&parse_filter_query(input));
    assert!(
        depth <= MAX_PLAN_DEPTH,
        "filter query produced a plan {depth} levels deep from {} bytes",
        input.len()
    );
    assert_eq!(
        depth_of(&parse_filter_query(input)),
        depth,
        "parse_filter_query is not deterministic for this input"
    );

    // The date grammar is reachable from the same box (`before:`, `since:`) and
    // is its own small parser.
    let _ = parse_date_text(input);
}

/// Depth of the plan tree, computed iteratively: recursing here would overflow
/// on exactly the input this check exists to catch.
fn depth_of(root: &SearchQuery) -> usize {
    let mut deepest = 0;
    let mut pending = vec![(root, 1usize)];
    while let Some((node, depth)) = pending.pop() {
        deepest = deepest.max(depth);
        match node {
            SearchQuery::And(children) | SearchQuery::Or(children) => {
                pending.extend(children.iter().map(|child| (child, depth + 1)));
            }
            SearchQuery::Not(child) => pending.push((child.as_ref(), depth + 1)),
            _ => {}
        }
    }
    deepest
}
