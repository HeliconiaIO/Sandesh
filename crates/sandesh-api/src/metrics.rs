//! The Prometheus text exposition format (Phase 7 A4).
//!
//! Rendering is kept apart from the handler because the format has rules that
//! are easy to break and silent when broken: a scraper that cannot parse a line
//! drops the whole scrape, and one that *can* parse a subtly wrong line records
//! a wrong number forever. The encoder is therefore a pure function over a
//! [`MetricsSnapshot`], which is what makes it testable without a server.
//!
//! ## What is deliberately not here
//!
//! No client library and no global registry. A registry is process-global
//! mutable state that every crate can reach, which is precisely the coupling
//! `ARCHITECTURE` §12 exists to prevent — and the figures this endpoint reports
//! are already in the database, so a parallel set of in-memory counters would
//! be a second source of truth that can disagree with the first. Everything
//! here is derived from one [`MetricsSource::snapshot`] read.
//!
//! [`MetricsSource::snapshot`]: sandesh_core::MetricsSource::snapshot

use sandesh_core::MetricsSnapshot;
use std::fmt::Write as _;

/// The content type a Prometheus scraper expects.
///
/// The version parameter is not decoration: a scraper uses it to choose a
/// parser, and omitting it makes some clients fall back to protobuf.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Renders a snapshot in the Prometheus text exposition format.
#[must_use]
pub fn render(snapshot: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(2048);

    gauge(
        &mut out,
        "sandesh_messages",
        "Messages stored, across every account.",
        snapshot.messages,
    );
    gauge(
        &mut out,
        "sandesh_mailboxes",
        "Mailboxes that exist, across every account.",
        snapshot.mailboxes,
    );
    gauge(
        &mut out,
        "sandesh_entries",
        "Placements of a message into a mailbox.",
        snapshot.entries,
    );
    gauge(
        &mut out,
        "sandesh_accounts",
        "Accounts that exist.",
        snapshot.accounts,
    );

    // A counter is emitted even with no samples, so a scraper sees the series
    // exist at zero rather than having it appear on first use — a series that
    // springs into being mid-scrape breaks `rate()` over the gap.
    out.push_str(
        "# HELP sandesh_deliveries_total Deliveries by source and per-recipient outcome.\n",
    );
    out.push_str("# TYPE sandesh_deliveries_total counter\n");
    for tally in &snapshot.deliveries {
        let _ = writeln!(
            out,
            "sandesh_deliveries_total{{source=\"{}\",outcome=\"{}\"}} {}",
            tally.source.as_str(),
            tally.outcome.as_str(),
            tally.count
        );
    }

    out.push_str("# HELP sandesh_connections_total Connections accepted, by protocol.\n");
    out.push_str("# TYPE sandesh_connections_total counter\n");
    for tally in &snapshot.connections {
        let _ = writeln!(
            out,
            "sandesh_connections_total{{protocol=\"{}\"}} {}",
            tally.protocol.as_str(),
            tally.connections
        );
    }

    out.push_str("# HELP sandesh_commands_total Client commands recorded, by protocol.\n");
    out.push_str("# TYPE sandesh_commands_total counter\n");
    for tally in &snapshot.connections {
        let _ = writeln!(
            out,
            "sandesh_commands_total{{protocol=\"{}\"}} {}",
            tally.protocol.as_str(),
            tally.commands
        );
    }

    // Seconds, not microseconds: Prometheus convention is base units, and a
    // dashboard that has to know this one server reports µs is a dashboard
    // that will eventually be wrong about it.
    out.push_str("# HELP sandesh_delivery_duration_seconds Time to accept and store a delivery.\n");
    out.push_str("# TYPE sandesh_delivery_duration_seconds histogram\n");
    for (bound_us, count) in &snapshot.delivery_durations.buckets {
        let _ = writeln!(
            out,
            "sandesh_delivery_duration_seconds_bucket{{le=\"{}\"}} {}",
            seconds(*bound_us),
            count
        );
    }
    // `+Inf` is mandatory and must equal the observation count: an observation
    // above the widest bound belongs to no finite bucket, so without it the
    // histogram silently loses the slowest deliveries — the ones worth seeing.
    let _ = writeln!(
        out,
        "sandesh_delivery_duration_seconds_bucket{{le=\"+Inf\"}} {}",
        snapshot.delivery_durations.count
    );
    let _ = writeln!(
        out,
        "sandesh_delivery_duration_seconds_sum {}",
        seconds(snapshot.delivery_durations.sum_us)
    );
    let _ = writeln!(
        out,
        "sandesh_delivery_duration_seconds_count {}",
        snapshot.delivery_durations.count
    );

    out
}

fn gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

/// Microseconds as seconds, to six decimal places.
///
/// Fixed precision rather than float formatting: `1e-3` is valid in the format
/// but a bucket bound is matched by a scraper as *text*, so a bound that
/// renders differently between scrapes reads as a different bucket.
fn seconds(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandesh_core::{
        DeliveryOutcome, DeliverySource, DeliveryTally, DurationHistogram, Protocol, ProtocolTally,
    };

    fn snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            messages: 3,
            mailboxes: 2,
            entries: 4,
            accounts: 1,
            deliveries: vec![DeliveryTally {
                source: DeliverySource::Smtp,
                outcome: DeliveryOutcome::Delivered,
                count: 3,
            }],
            connections: vec![ProtocolTally {
                protocol: Protocol::Smtp,
                connections: 2,
                commands: 9,
            }],
            delivery_durations: DurationHistogram::from_micros([500, 1_500]),
        }
    }

    /// Every `# TYPE` must have a matching series, and every series a `# TYPE`.
    /// A sample with no declared type is parsed as untyped and silently loses
    /// its histogram or counter semantics.
    #[test]
    fn every_series_is_typed() {
        let rendered = render(&snapshot());

        let declared: Vec<&str> = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
            .filter_map(|line| line.split_whitespace().next())
            .collect();

        for line in rendered.lines().filter(|line| !line.starts_with('#')) {
            let Some(name) = line.split(['{', ' ']).next() else {
                continue;
            };
            // Histogram samples carry the suffixes the base name declares.
            let base = name
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count");
            assert!(
                declared.iter().any(|d| *d == name || *d == base),
                "{name} has no # TYPE declaration"
            );
        }
    }

    #[test]
    fn the_histogram_carries_a_mandatory_inf_bucket_equal_to_the_count() {
        let rendered = render(&snapshot());

        assert!(
            rendered.contains("sandesh_delivery_duration_seconds_bucket{le=\"+Inf\"} 2"),
            "missing or wrong +Inf bucket:\n{rendered}"
        );
        assert!(rendered.contains("sandesh_delivery_duration_seconds_count 2"));
    }

    /// Microseconds reported as seconds. 1500µs is 0.0015s, and reporting
    /// `1500` here would overstate every latency by a millionfold.
    #[test]
    fn durations_are_reported_in_seconds() {
        assert_eq!(seconds(1_500), "0.001500");
        assert_eq!(seconds(0), "0.000000");
        assert_eq!(seconds(2_000_000), "2.000000");

        let rendered = render(&snapshot());
        assert!(rendered.contains("sandesh_delivery_duration_seconds_sum 0.002000"));
    }

    #[test]
    fn labelled_series_carry_their_labels() {
        let rendered = render(&snapshot());

        assert!(
            rendered.contains("sandesh_deliveries_total{source=\"smtp\",outcome=\"delivered\"} 3")
        );
        assert!(rendered.contains("sandesh_connections_total{protocol=\"smtp\"} 2"));
        assert!(rendered.contains("sandesh_commands_total{protocol=\"smtp\"} 9"));
    }

    /// An instance that has done nothing still renders every series, so a
    /// dashboard built against a busy server does not break against a fresh one.
    #[test]
    fn an_empty_snapshot_still_declares_every_series() {
        let rendered = render(&MetricsSnapshot::default());

        for name in [
            "sandesh_messages",
            "sandesh_mailboxes",
            "sandesh_entries",
            "sandesh_accounts",
            "sandesh_deliveries_total",
            "sandesh_connections_total",
            "sandesh_commands_total",
            "sandesh_delivery_duration_seconds",
        ] {
            assert!(
                rendered.contains(&format!("# TYPE {name} ")),
                "{name} is missing from an empty scrape"
            );
        }
    }

    /// The exposition format is line-oriented and newline-terminated; a final
    /// line without its newline is a parse error in some scrapers.
    #[test]
    fn the_document_is_newline_terminated() {
        let rendered = render(&snapshot());
        assert!(rendered.ends_with('\n'));
        assert!(!rendered.contains("\n\n"), "blank lines are not valid");
    }
}
