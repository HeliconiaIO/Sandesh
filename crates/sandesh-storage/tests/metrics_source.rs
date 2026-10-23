//! The readings behind `/metrics` (Phase 7 A4).
//!
//! These assert the *arithmetic*, which is where a metrics endpoint goes wrong
//! quietly: a scrape that parses and serves a plausible-looking wrong number is
//! indistinguishable from a correct one until someone acts on it.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    ConnectionDisposition, ConnectionId, ConnectionRecord, ConnectionStore, Delivery, DeliveryId,
    DeliveryOutcome, DeliverySource, DeliveryStore, DurationHistogram, EmailAddress, Envelope,
    MetricsSource, Protocol, RecipientOutcome, Timestamp,
};
use sandesh_storage::{
    EventRecorder, SqliteConnectionStore, SqliteDeliveryStore, SqliteMetricsSource,
};

fn metrics(harness: &Harness) -> SqliteMetricsSource {
    SqliteMetricsSource::new(harness.db.pool().clone())
}

fn connection_with(protocol: Protocol, transcript: Vec<String>) -> ConnectionRecord {
    let now = Timestamp::now();
    ConnectionRecord {
        id: ConnectionId::new(),
        protocol,
        peer: Some(String::from("127.0.0.1:54321")),
        encrypted: false,
        opened_at: now,
        closed_at: Some(now),
        disposition: Some(ConnectionDisposition::Closed),
        transcript,
        truncated: false,
    }
}

fn address(value: &str) -> EmailAddress {
    EmailAddress::parse_lenient(value)
}

fn delivery_with(source: DeliverySource, duration_us: i64) -> Delivery {
    Delivery {
        id: DeliveryId::new(),
        message_id: None,
        source,
        envelope: Envelope {
            mail_from: Some(address("sender@example.test")),
            rcpt_to: Vec::new(),
            helo_domain: None,
            peer: None,
            tls: None,
            auth_identity: None,
        },
        outcomes: Vec::new(),
        received_at: Timestamp::now(),
        duration_us,
        connection_id: None,
        rules_applied: Vec::new(),
        replay_of: None,
    }
}

#[tokio::test]
async fn an_empty_database_reports_zeroes_rather_than_failing() {
    let harness = Harness::new().await;

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    assert_eq!(snapshot.messages, 0);
    assert_eq!(snapshot.mailboxes, 0);
    assert_eq!(snapshot.entries, 0);
    assert!(snapshot.deliveries.is_empty());
    assert!(snapshot.connections.is_empty());
    assert_eq!(snapshot.delivery_durations.count, 0);
}

/// The case the SQL is written for, and the reason it keys on line position.
///
/// `Transcript::line` renders the client's own bytes after the `C:` marker, so
/// a client that puts `C:` in an argument would inflate a naive occurrence
/// count. Three commands are sent here and three must be reported.
#[tokio::test]
async fn commands_are_counted_per_line_not_per_occurrence() {
    let harness = Harness::new().await;
    let connections = SqliteConnectionStore::new(harness.db.pool().clone());

    connections
        .record(&connection_with(
            Protocol::Smtp,
            vec![
                String::from("C: EHLO localhost"),
                String::from("S: 250 sandesh.test"),
                // "C:" appears inside the argument, not as a marker.
                String::from("C: MAIL FROM:<user@C:host>"),
                String::from("S: 250 ok"),
                String::from("C: QUIT"),
            ],
        ))
        .await
        .expect("record");

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    let smtp = snapshot
        .connections
        .iter()
        .find(|tally| tally.protocol == Protocol::Smtp)
        .expect("an SMTP tally");

    assert_eq!(smtp.connections, 1);
    assert_eq!(
        smtp.commands, 3,
        "the embedded C: in an argument must not count as a command"
    );
}

/// A transcript whose very first line is a client line has no separator before
/// it, which an unadjusted separator count would miss.
#[tokio::test]
async fn the_first_line_counts_when_the_client_speaks_first() {
    let harness = Harness::new().await;
    let connections = SqliteConnectionStore::new(harness.db.pool().clone());

    connections
        .record(&connection_with(
            Protocol::Imap,
            vec![
                String::from("C: a001 LOGIN user pass"),
                String::from("S: a001 OK"),
            ],
        ))
        .await
        .expect("record");
    // A server-first conversation, the SMTP shape: the greeting precedes
    // anything the client says.
    connections
        .record(&connection_with(
            Protocol::Pop3,
            vec![String::from("S: +OK ready"), String::from("C: QUIT")],
        ))
        .await
        .expect("record");

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    let imap = snapshot
        .connections
        .iter()
        .find(|tally| tally.protocol == Protocol::Imap)
        .expect("an IMAP tally");
    assert_eq!(imap.commands, 1);

    let pop3 = snapshot
        .connections
        .iter()
        .find(|tally| tally.protocol == Protocol::Pop3)
        .expect("a POP3 tally");
    assert_eq!(pop3.commands, 1);
}

#[tokio::test]
async fn an_empty_or_absent_transcript_counts_no_commands() {
    let harness = Harness::new().await;
    let connections = SqliteConnectionStore::new(harness.db.pool().clone());

    connections
        .record(&connection_with(Protocol::Smtp, Vec::new()))
        .await
        .expect("record");

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    let smtp = snapshot
        .connections
        .iter()
        .find(|tally| tally.protocol == Protocol::Smtp)
        .expect("an SMTP tally");

    assert_eq!(smtp.connections, 1, "the connection itself still counts");
    assert_eq!(smtp.commands, 0);
}

/// A rejected transaction stores no recipient row and no message. It must still
/// appear in the tally — recording it and then not counting it would be the
/// same invisibility A1 set out to fix.
#[tokio::test]
async fn a_rejected_transaction_with_no_recipients_is_still_counted() {
    let harness = Harness::new().await;
    let deliveries = SqliteDeliveryStore::new(
        harness.db.pool().clone(),
        EventRecorder::new(harness.sink.clone()),
    );

    deliveries
        .record(&delivery_with(DeliverySource::Smtp, 1_500))
        .await
        .expect("record");

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    let tally = snapshot
        .deliveries
        .iter()
        .find(|tally| tally.source == DeliverySource::Smtp)
        .expect("an SMTP tally");

    assert_eq!(tally.outcome, DeliveryOutcome::Rejected);
    assert_eq!(tally.count, 1);
}

#[tokio::test]
async fn durations_land_in_their_buckets_and_sum() {
    let harness = Harness::new().await;
    let deliveries = SqliteDeliveryStore::new(
        harness.db.pool().clone(),
        EventRecorder::new(harness.sink.clone()),
    );

    for duration_us in [500_i64, 1_500, 30_000] {
        deliveries
            .record(&delivery_with(DeliverySource::Smtp, duration_us))
            .await
            .expect("record");
    }

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");
    let histogram = snapshot.delivery_durations;

    assert_eq!(histogram.count, 3);
    assert_eq!(histogram.sum_us, 32_000);

    // Cumulative: the 1ms bucket holds only the 500µs delivery; the 2ms bucket
    // holds that one and the 1.5ms one.
    let at = |bound: u64| {
        histogram
            .buckets
            .iter()
            .find(|(b, _)| *b == bound)
            .map(|(_, count)| *count)
            .expect("bucket")
    };
    assert_eq!(at(1_000), 1);
    assert_eq!(at(2_000), 2);
    assert_eq!(at(50_000), 3);
}

/// Buckets are cumulative and non-decreasing, and the last equals the total —
/// the invariant a Prometheus histogram is required to hold.
#[test]
fn the_histogram_is_cumulative() {
    let histogram = DurationHistogram::from_micros([10, 3_000, 3_000, 900_000]);

    assert_eq!(histogram.count, 4);
    let mut previous = 0;
    for (_, count) in &histogram.buckets {
        assert!(*count >= previous, "bucket counts must not decrease");
        previous = *count;
    }
    assert_eq!(
        histogram.buckets.last().expect("a bucket").1,
        histogram.count,
        "every observation is under the widest bound"
    );
}

/// A negative duration means the clock moved. Folding it into the fastest
/// bucket would report a delivery that never happened.
#[test]
fn a_negative_duration_is_ignored_rather_than_clamped() {
    let histogram = DurationHistogram::from_micros([-1, 500]);

    assert_eq!(histogram.count, 1);
    assert_eq!(histogram.sum_us, 500);
}

#[tokio::test]
async fn recipient_outcomes_are_counted_individually() {
    let harness = Harness::new().await;
    let deliveries = SqliteDeliveryStore::new(
        harness.db.pool().clone(),
        EventRecorder::new(harness.sink.clone()),
    );

    // One transaction, two recipients, two different outcomes. Counting by
    // delivery would have to pick one and lose the other.
    let mut delivery = delivery_with(DeliverySource::Smtp, 2_000);
    delivery.outcomes = vec![
        RecipientOutcome {
            rcpt_to: address("known@example.test"),
            resolved_account: None,
            entry_id: None,
            outcome: DeliveryOutcome::Delivered,
            reason: None,
        },
        RecipientOutcome {
            rcpt_to: address("unknown@example.test"),
            resolved_account: None,
            entry_id: None,
            outcome: DeliveryOutcome::Rejected,
            reason: Some(String::from("unknown recipient")),
        },
    ];
    deliveries.record(&delivery).await.expect("record");

    let snapshot = metrics(&harness).snapshot().await.expect("snapshot");

    let count_for = |outcome: DeliveryOutcome| {
        snapshot
            .deliveries
            .iter()
            .find(|tally| tally.outcome == outcome)
            .map_or(0, |tally| tally.count)
    };

    assert_eq!(count_for(DeliveryOutcome::Delivered), 1);
    assert_eq!(count_for(DeliveryOutcome::Rejected), 1);
}
