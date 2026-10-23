//! Header analysis (`ROADMAP.md` Phase 7 A3).
//!
//! Turns the flat header list into the three things a developer debugging mail
//! actually asks for: how the message travelled, what authentication headers
//! upstream left behind, and whether a spam filter said anything.
//!
//! ## Parsed, never verified
//!
//! This module **never verifies** a DKIM signature, an SPF decision or a DMARC
//! policy. It parses the fields those mechanisms wrote and reports them as
//! claims made by whoever wrote the header. Sandesh is a development mail
//! server: a verifier here would be a liability, because a developer would
//! reasonably read a green tick as meaning their production mail will pass, and
//! it means nothing of the kind. Every type in this module is therefore named
//! for what it holds, not for a verdict, and the one thing resembling a verdict
//! — [`AuthResult::result`] — is the literal text the upstream server wrote.
//!
//! ## Robustness
//!
//! Analysis never fails and never panics. A malformed `Received:` field yields
//! a hop with empty components rather than a dropped hop, because the
//! unparseable hop is usually the interesting one. Unknown tags in a DKIM
//! signature are preserved verbatim for the same reason.

use crate::message::Header;
use crate::time::Timestamp;

/// Everything the analysis of one message's headers found.
///
/// Every field may be empty. An empty [`HeaderAnalysis::hops`] means no
/// `Received:` field was present, which is normal for a message injected
/// through the API rather than delivered over SMTP.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeaderAnalysis {
    /// The `Received:` chain, oldest hop first.
    ///
    /// Note the reversal: `Received:` fields are prepended by each hop, so the
    /// header order is newest-first. This list is chronological, which is the
    /// order a timeline is read in.
    pub hops: Vec<ReceivedHop>,
    /// Parsed `DKIM-Signature:` fields, in header order.
    pub dkim: Vec<DkimSignature>,
    /// Results extracted from `Authentication-Results:` fields.
    pub auth_results: Vec<AuthResult>,
    /// `Received-SPF:` fields, in header order.
    pub spf: Vec<SpfRecord>,
    /// `X-Spam-*` and `X-Spam` headers, surfaced as-is.
    pub spam: Vec<SpamHeader>,
}

/// One hop of the `Received:` chain.
///
/// The RFC 5321 §4.4 grammar is advisory in practice — real relays emit
/// variations no parser matches exhaustively — so each clause is extracted
/// independently and left `None` when absent rather than failing the hop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceivedHop {
    /// Zero-based position in chronological order; hop 0 is the oldest.
    pub index: usize,
    /// The `from` clause: the host that handed the message over.
    pub from: Option<String>,
    /// The `by` clause: the host that received it.
    pub by: Option<String>,
    /// The `with` clause: the protocol used, e.g. `ESMTPS`.
    pub with: Option<String>,
    /// The `for` clause: the envelope recipient this hop saw.
    pub for_recipient: Option<String>,
    /// The `id` clause: the receiving host's queue id.
    pub id: Option<String>,
    /// The timestamp after the final semicolon, when it parses.
    pub timestamp: Option<Timestamp>,
    /// Microseconds between the previous hop's timestamp and this one.
    ///
    /// `None` for the first hop and wherever either timestamp is missing or
    /// unparseable. May be negative: relay clocks disagree, and hiding that
    /// would conceal exactly the kind of fault this view exists to show.
    pub delay_micros: Option<i64>,
    /// The whole field value, for when the parse missed something.
    pub raw: String,
}

/// A parsed `DKIM-Signature:` field.
///
/// Tag-value pairs per RFC 6376 §3.5. Fields are exposed both individually and,
/// in [`DkimSignature::tags`], as the complete list — a signature carrying a tag
/// this type does not name should still be visible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DkimSignature {
    /// `v=` — version, normally `1`.
    pub version: Option<String>,
    /// `a=` — the signing algorithm, e.g. `rsa-sha256`.
    pub algorithm: Option<String>,
    /// `d=` — the signing domain.
    pub domain: Option<String>,
    /// `s=` — the selector naming the public key record.
    pub selector: Option<String>,
    /// `c=` — the canonicalisation, e.g. `relaxed/relaxed`.
    pub canonicalization: Option<String>,
    /// `h=` — the signed header names, in the order signed.
    pub signed_headers: Vec<String>,
    /// `bh=` — the body hash.
    pub body_hash: Option<String>,
    /// `t=` — the signature timestamp, when it parses as seconds since epoch.
    pub timestamp: Option<Timestamp>,
    /// `x=` — the expiry, when it parses.
    pub expiration: Option<Timestamp>,
    /// Every tag as written, in order, including ones not named above.
    pub tags: Vec<(String, String)>,
}

/// One method's result from an `Authentication-Results:` field.
///
/// The `result` is the upstream server's word, reproduced. Sandesh does not
/// check it and does not agree or disagree with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    /// The authserv-id: the host claiming to have performed the check.
    pub authserv_id: String,
    /// The method, e.g. `dkim`, `spf`, `dmarc`.
    pub method: String,
    /// The result as written, e.g. `pass`, `fail`, `none`.
    pub result: String,
    /// Trailing `key=value` properties for this method, in order.
    pub properties: Vec<(String, String)>,
}

/// A `Received-SPF:` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpfRecord {
    /// The result keyword, e.g. `pass`, `softfail`, `none`.
    pub result: String,
    /// The free-text remainder, which usually names the checking host and IP.
    pub detail: String,
}

/// An `X-Spam-*` header, surfaced without interpretation.
///
/// Scores differ per filter and per configuration, so no threshold is applied
/// and no message is labelled spam here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpamHeader {
    /// The field name as it appeared.
    pub name: String,
    /// The decoded value.
    pub value: String,
}

impl HeaderAnalysis {
    /// Analyses a message's headers.
    ///
    /// Total: every input produces an analysis, and headers that do not parse
    /// are reported with their raw text rather than discarded.
    #[must_use]
    pub fn analyze(headers: &[Header]) -> Self {
        let mut analysis = Self::default();

        // `Received:` fields are prepended, so header order is newest-first and
        // the chronological chain is that order reversed.
        let mut hops: Vec<ReceivedHop> = headers
            .iter()
            .filter(|h| h.name_matches("Received"))
            .map(|h| parse_received(&h.decoded_value))
            .collect();
        hops.reverse();

        let mut previous: Option<Timestamp> = None;
        for (index, hop) in hops.iter_mut().enumerate() {
            hop.index = index;
            hop.delay_micros = match (previous, hop.timestamp) {
                (Some(prev), Some(now)) => Some(now.as_micros() - prev.as_micros()),
                _ => None,
            };
            if hop.timestamp.is_some() {
                previous = hop.timestamp;
            }
        }
        analysis.hops = hops;

        for header in headers {
            if header.name_matches("DKIM-Signature") {
                analysis.dkim.push(parse_dkim(&header.decoded_value));
            } else if header.name_matches("Authentication-Results") {
                analysis
                    .auth_results
                    .extend(parse_authentication_results(&header.decoded_value));
            } else if header.name_matches("Received-SPF") {
                analysis.spf.push(parse_spf(&header.decoded_value));
            } else if is_spam_header(&header.name) {
                analysis.spam.push(SpamHeader {
                    name: header.name.clone(),
                    value: header.decoded_value.clone(),
                });
            }
        }

        analysis
    }

    /// Whether anything at all was found.
    ///
    /// A fully empty analysis is a normal outcome — an API-injected message has
    /// no hops and no authentication headers — and the UI says so rather than
    /// rendering four empty panels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
            && self.dkim.is_empty()
            && self.auth_results.is_empty()
            && self.spf.is_empty()
            && self.spam.is_empty()
    }
}

/// Whether a field name is one of the SpamAssassin-style `X-Spam` headers.
fn is_spam_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "x-spam" || lower.starts_with("x-spam-")
}

/// Extracts the clauses of one `Received:` field.
///
/// Clause keywords are matched only at token boundaries, so a hostname
/// containing `by` or an id containing `for` does not start a false clause.
fn parse_received(value: &str) -> ReceivedHop {
    let mut hop = ReceivedHop {
        raw: value.to_string(),
        ..ReceivedHop::default()
    };

    // The date, if any, follows the last semicolon (RFC 5321 §4.4). Splitting
    // there first keeps a semicolon inside a comment from stealing the date.
    let (clauses, date) = match value.rfind(';') {
        Some(at) => (&value[..at], Some(value[at + 1..].trim())),
        None => (value, None),
    };

    hop.timestamp = date.and_then(parse_date);

    let tokens: Vec<&str> = clauses.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        let keyword = tokens[index].to_ascii_lowercase();
        let slot = match keyword.as_str() {
            "from" => Some(&mut hop.from),
            "by" => Some(&mut hop.by),
            "with" => Some(&mut hop.with),
            "for" => Some(&mut hop.for_recipient),
            "id" => Some(&mut hop.id),
            _ => None,
        };

        match slot {
            // A clause value runs to the next clause keyword. Only the first
            // occurrence is taken: a relay that writes `by` twice is reporting
            // its own first answer, and overwriting would show the last.
            Some(slot) => {
                let start = index + 1;
                let mut end = start;
                while end < tokens.len() && !is_clause_keyword(tokens[end]) {
                    end += 1;
                }
                if slot.is_none() && start < end {
                    *slot = Some(tokens[start..end].join(" "));
                }
                index = end;
            }
            None => index += 1,
        }
    }

    hop
}

/// Whether a token starts a new `Received:` clause.
fn is_clause_keyword(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "from" | "by" | "via" | "with" | "id" | "for"
    )
}

/// Parses an RFC 5322 date-time, tolerating a trailing `(TZ)` comment.
fn parse_date(text: &str) -> Option<Timestamp> {
    let trimmed = match text.find('(') {
        Some(at) => text[..at].trim(),
        None => text.trim(),
    };
    let parsed =
        time::OffsetDateTime::parse(trimmed, &time::format_description::well_known::Rfc2822)
            .ok()?;
    let micros = parsed.unix_timestamp_nanos() / 1_000;
    Some(Timestamp::from_micros(i64::try_from(micros).ok()?))
}

/// Parses the tag-value list of a `DKIM-Signature:` field.
fn parse_dkim(value: &str) -> DkimSignature {
    let mut signature = DkimSignature::default();

    for pair in value.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        // Split on the first `=` only: base64 tag values contain `=` padding.
        let Some((tag, tag_value)) = pair.split_once('=') else {
            continue;
        };
        let tag = tag.trim();
        // Folding whitespace is not part of a tag value (RFC 6376 §3.2), and
        // leaving it in would corrupt the base64 of `b=` and `bh=`.
        let tag_value: String = tag_value.split_whitespace().collect();

        match tag {
            "v" => signature.version = Some(tag_value.clone()),
            "a" => signature.algorithm = Some(tag_value.clone()),
            "d" => signature.domain = Some(tag_value.clone()),
            "s" => signature.selector = Some(tag_value.clone()),
            "c" => signature.canonicalization = Some(tag_value.clone()),
            "h" => {
                signature.signed_headers = tag_value
                    .split(':')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "bh" => signature.body_hash = Some(tag_value.clone()),
            "t" => signature.timestamp = parse_epoch_seconds(&tag_value),
            "x" => signature.expiration = parse_epoch_seconds(&tag_value),
            _ => {}
        }

        signature.tags.push((tag.to_string(), tag_value));
    }

    signature
}

/// Parses a DKIM `t=`/`x=` value: seconds since the Unix epoch.
fn parse_epoch_seconds(text: &str) -> Option<Timestamp> {
    let seconds: i64 = text.parse().ok()?;
    seconds.checked_mul(1_000_000).map(Timestamp::from_micros)
}

/// Parses an `Authentication-Results:` field into one entry per method.
///
/// RFC 8601 §2.2: an authserv-id, then semicolon-separated `method=result`
/// clauses each optionally followed by `ptype.property=value` items.
fn parse_authentication_results(value: &str) -> Vec<AuthResult> {
    let mut parts = value.split(';');
    let authserv_id = parts
        .next()
        .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
        .unwrap_or_default();

    let mut results = Vec::new();
    for clause in parts {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }

        let mut items = clause.split_whitespace();
        let Some(first) = items.next() else { continue };
        let Some((method, result)) = first.split_once('=') else {
            continue;
        };

        let properties = items
            .filter_map(|item| {
                item.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
            })
            .collect();

        results.push(AuthResult {
            authserv_id: authserv_id.clone(),
            method: method.to_ascii_lowercase(),
            result: result.to_ascii_lowercase(),
            properties,
        });
    }

    results
}

/// Parses a `Received-SPF:` field: a result keyword, then free text.
fn parse_spf(value: &str) -> SpfRecord {
    let trimmed = value.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((result, detail)) => SpfRecord {
            result: result.to_ascii_lowercase(),
            detail: detail.trim().to_string(),
        },
        None => SpfRecord {
            result: trimmed.to_ascii_lowercase(),
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::ByteRange;

    fn header(name: &str, value: &str) -> Header {
        Header {
            name: name.to_string(),
            raw_value: value.as_bytes().to_vec(),
            decoded_value: value.to_string(),
            range: ByteRange::empty_at(0),
        }
    }

    #[test]
    fn no_headers_yields_an_empty_analysis() {
        let analysis = HeaderAnalysis::analyze(&[]);
        assert!(analysis.is_empty());
    }

    #[test]
    fn hops_are_chronological_not_header_order() {
        // Newest-first in the header block, as a real relay chain arrives.
        let headers = vec![
            header(
                "Received",
                "from relay.example by mx.example with ESMTPS id B2; Tue, 1 Jul 2025 10:00:05 +0000",
            ),
            header(
                "Received",
                "from origin.example by relay.example with ESMTP id A1; Tue, 1 Jul 2025 10:00:00 +0000",
            ),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.hops.len(), 2);
        assert_eq!(analysis.hops[0].index, 0);
        assert_eq!(analysis.hops[0].from.as_deref(), Some("origin.example"));
        assert_eq!(analysis.hops[1].from.as_deref(), Some("relay.example"));
    }

    #[test]
    fn per_hop_delay_is_measured_against_the_previous_hop() {
        let headers = vec![
            header(
                "Received",
                "from relay.example by mx.example; Tue, 1 Jul 2025 10:00:05 +0000",
            ),
            header(
                "Received",
                "from origin.example by relay.example; Tue, 1 Jul 2025 10:00:00 +0000",
            ),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(
            analysis.hops[0].delay_micros, None,
            "first hop has no predecessor"
        );
        assert_eq!(analysis.hops[1].delay_micros, Some(5_000_000));
    }

    #[test]
    fn a_backwards_clock_reports_a_negative_delay_rather_than_hiding_it() {
        let headers = vec![
            header("Received", "from b by c; Tue, 1 Jul 2025 10:00:00 +0000"),
            header("Received", "from a by b; Tue, 1 Jul 2025 10:00:05 +0000"),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.hops[1].delay_micros, Some(-5_000_000));
    }

    #[test]
    fn a_malformed_received_field_still_produces_a_hop() {
        let headers = vec![header("Received", "this is not a received field at all")];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(
            analysis.hops.len(),
            1,
            "the unparseable hop is the interesting one"
        );
        assert_eq!(analysis.hops[0].from, None);
        assert_eq!(analysis.hops[0].timestamp, None);
        assert_eq!(analysis.hops[0].raw, "this is not a received field at all");
    }

    #[test]
    fn clause_keywords_are_matched_at_token_boundaries() {
        // `forwarder.example` begins with `for`; `bytes.example` begins with `by`.
        let headers = vec![header(
            "Received",
            "from forwarder.example by bytes.example with ESMTP",
        )];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.hops[0].from.as_deref(), Some("forwarder.example"));
        assert_eq!(analysis.hops[0].by.as_deref(), Some("bytes.example"));
        assert_eq!(analysis.hops[0].for_recipient, None);
    }

    #[test]
    fn a_timezone_comment_after_the_date_still_parses() {
        let headers = vec![header(
            "Received",
            "from a by b; Tue, 1 Jul 2025 10:00:00 +0000 (UTC)",
        )];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert!(analysis.hops[0].timestamp.is_some());
    }

    #[test]
    fn dkim_tags_are_parsed_and_unknown_ones_preserved() {
        let headers = vec![header(
            "DKIM-Signature",
            "v=1; a=rsa-sha256; d=example.com; s=sel1; c=relaxed/relaxed; \
             h=from:to:subject; bh=abc123=; t=1751364000; z=custom",
        )];

        let analysis = HeaderAnalysis::analyze(&headers);
        let signature = &analysis.dkim[0];

        assert_eq!(signature.version.as_deref(), Some("1"));
        assert_eq!(signature.algorithm.as_deref(), Some("rsa-sha256"));
        assert_eq!(signature.domain.as_deref(), Some("example.com"));
        assert_eq!(signature.selector.as_deref(), Some("sel1"));
        assert_eq!(signature.signed_headers, ["from", "to", "subject"]);
        assert_eq!(signature.body_hash.as_deref(), Some("abc123="));
        assert_eq!(
            signature.timestamp,
            Some(Timestamp::from_micros(1_751_364_000_000_000))
        );
        assert!(
            signature
                .tags
                .iter()
                .any(|(k, v)| k == "z" && v == "custom"),
            "an unnamed tag must still be visible"
        );
    }

    #[test]
    fn folded_base64_in_a_dkim_tag_is_rejoined() {
        let headers = vec![header("DKIM-Signature", "bh=abc\r\n  123; b=xyz\r\n  789")];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.dkim[0].body_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn authentication_results_yields_one_entry_per_method() {
        let headers = vec![header(
            "Authentication-Results",
            "mx.example.com; dkim=pass header.d=example.com; spf=fail smtp.mailfrom=a@example.net; dmarc=none",
        )];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.auth_results.len(), 3);
        assert_eq!(analysis.auth_results[0].authserv_id, "mx.example.com");
        assert_eq!(analysis.auth_results[0].method, "dkim");
        assert_eq!(analysis.auth_results[0].result, "pass");
        assert_eq!(
            analysis.auth_results[0].properties,
            [("header.d".to_string(), "example.com".to_string())]
        );
        assert_eq!(analysis.auth_results[1].method, "spf");
        assert_eq!(analysis.auth_results[1].result, "fail");
        assert_eq!(analysis.auth_results[2].method, "dmarc");
    }

    #[test]
    fn received_spf_splits_the_result_from_its_detail() {
        let headers = vec![header(
            "Received-SPF",
            "Pass (mailfrom) identity=mailfrom; client-ip=192.0.2.1",
        )];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.spf[0].result, "pass");
        assert!(analysis.spf[0].detail.starts_with("(mailfrom)"));
    }

    #[test]
    fn spam_headers_are_surfaced_without_interpretation() {
        let headers = vec![
            header("X-Spam-Status", "No, score=-1.9"),
            header("X-Spam-Level", ""),
            header("X-Mailer", "not a spam header"),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.spam.len(), 2);
        assert_eq!(analysis.spam[0].name, "X-Spam-Status");
        assert_eq!(analysis.spam[0].value, "No, score=-1.9");
    }

    #[test]
    fn header_names_are_matched_case_insensitively() {
        let headers = vec![
            header("received", "from a by b"),
            header("dkim-signature", "v=1; d=example.com"),
            header("x-SPAM-flag", "YES"),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert_eq!(analysis.hops.len(), 1);
        assert_eq!(analysis.dkim.len(), 1);
        assert_eq!(analysis.spam.len(), 1);
    }

    #[test]
    fn a_message_with_no_authentication_headers_analyses_cleanly() {
        let headers = vec![
            header("From", "a@example.com"),
            header("Subject", "hello"),
            header("Received", "from a by b; Tue, 1 Jul 2025 10:00:00 +0000"),
        ];

        let analysis = HeaderAnalysis::analyze(&headers);

        assert!(!analysis.is_empty(), "a hop was found");
        assert!(analysis.dkim.is_empty());
        assert!(analysis.auth_results.is_empty());
        assert!(analysis.spf.is_empty());
    }

    /// Reads a fixture from `testdata/`, preserving its bytes exactly.
    fn fixture(relative: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(relative);
        std::fs::read(&path).unwrap_or_else(|error| {
            unreachable!("read {}: {error}", path.display());
        })
    }

    /// Analyses a fixture end to end, through the real parser.
    fn analyze_fixture(relative: &str) -> HeaderAnalysis {
        let raw = fixture(relative);
        let parsed = crate::parser::parse(&raw);
        HeaderAnalysis::analyze(&parsed.headers)
    }

    #[test]
    fn the_authentication_fixture_analyses_end_to_end() {
        // The fixture exists for this feature ("for header analysis") and is
        // harder than a hand-built case: tab folding, a `+0000 (UTC)` comment,
        // and an SPF clause whose parenthesised text contains a semicolon.
        let analysis = analyze_fixture("real_world/with-authentication-headers.eml");

        assert_eq!(analysis.hops.len(), 2, "both Received fields became hops");
        assert_eq!(
            analysis.hops[0].from.as_deref(),
            Some("app-01.internal (localhost [127.0.0.1])"),
            "the inner hop is oldest"
        );
        assert_eq!(analysis.hops[1].by.as_deref(), Some("mx.example.test"));
        assert_eq!(
            analysis.hops[1].for_recipient.as_deref(),
            Some("<dev@example.test>")
        );

        // Three seconds elapsed between the two hops, per their own stamps.
        assert_eq!(analysis.hops[1].delay_micros, Some(3_000_000));

        let signature = &analysis.dkim[0];
        assert_eq!(signature.domain.as_deref(), Some("example.net"));
        assert_eq!(signature.selector.as_deref(), Some("selector1"));
        assert_eq!(signature.algorithm.as_deref(), Some("rsa-sha256"));
        assert_eq!(
            signature.body_hash.as_deref(),
            Some("2jmj7l5rSw0yVb+vlWAYkK/YBwk="),
            "a tab-folded base64 tag must rejoin exactly"
        );
        assert_eq!(signature.signed_headers, ["From", "To", "Subject", "Date"]);
        assert!(signature.timestamp.is_some(), "t= parsed");

        let methods: Vec<&str> = analysis
            .auth_results
            .iter()
            .map(|r| r.method.as_str())
            .collect();
        assert!(
            methods.contains(&"dkim") && methods.contains(&"spf") && methods.contains(&"dmarc"),
            "all three methods were found: {methods:?}"
        );
    }

    #[test]
    fn every_fixture_in_the_corpus_analyses_without_panicking() {
        // Analysis is total (see the module docs). Running the whole corpus is
        // what makes that claim mean something: these are the malformed and
        // adversarial messages the parser is hardened against, and a header
        // analyser that panics on one would take the API down with it.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata");

        let mut checked = 0_usize;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "eml") {
                    let Ok(raw) = std::fs::read(&path) else {
                        continue;
                    };
                    let parsed = crate::parser::parse(&raw);
                    let analysis = HeaderAnalysis::analyze(&parsed.headers);

                    // Every hop index is its position, and a delay never exists
                    // without the two timestamps that define it.
                    for (index, hop) in analysis.hops.iter().enumerate() {
                        assert_eq!(hop.index, index, "hop index in {}", path.display());
                        if hop.delay_micros.is_some() {
                            assert!(
                                hop.timestamp.is_some(),
                                "a delay without a timestamp in {}",
                                path.display()
                            );
                        }
                    }
                    checked += 1;
                }
            }
        }

        assert!(
            checked >= 100,
            "the corpus is ~101 fixtures; only {checked} were analysed"
        );
    }

    #[test]
    fn a_malformed_received_chain_degrades_clause_by_clause() {
        // The plan's hard case. Every clause here is truncated, empty or
        // unparseable, and the requirement is that nothing is dropped: the hop
        // that defeats the parser is the one worth showing.
        let analysis = analyze_fixture("malformed/broken-received-chain.eml");

        assert_eq!(
            analysis.hops.len(),
            5,
            "every Received field became a hop, however broken"
        );

        // Chronological order reverses the header block, so the last-written
        // field is the oldest hop.
        assert_eq!(analysis.hops[0].from.as_deref(), Some("c.example"));

        // A hop whose date does not parse carries no timestamp and therefore
        // no delay, rather than a fabricated one.
        let Some(undated) = analysis.hops.iter().find(|h| h.id.as_deref() == Some("Z9")) else {
            unreachable!("the hop with an unparseable date survived");
        };
        assert_eq!(undated.timestamp, None);
        assert_eq!(undated.delay_micros, None);

        // The wholly unparseable field keeps its raw text.
        assert!(
            analysis
                .hops
                .iter()
                .any(|h| h.raw == "this is not a received field at all"),
            "the unparseable hop is preserved verbatim"
        );

        // A `from` with nothing after it is absent, not an empty string.
        assert!(
            analysis.hops.iter().any(|h| h.from.is_none()),
            "a truncated clause reads as absent"
        );

        // Empty auth headers must not invent entries.
        assert!(analysis.auth_results.is_empty(), "no method, no claim");
        assert_eq!(analysis.dkim.len(), 1, "the empty signature is still shown");
        assert_eq!(analysis.dkim[0].domain.as_deref(), Some(""));
        assert!(analysis.dkim[0].signed_headers.is_empty());
    }
}
