//! Raw byte fidelity — the invariant everything else rests on.
//!
//! `stored_raw_bytes == originally_received_bytes`, for every message, always.
//! MIME testing, malformed-message debugging, header inspection and DKIM
//! analysis all become worthless the moment this stops holding.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    BlobStore, DeliverySource, Envelope, MessageFlags, MessageStore, NewMessage, NewPlacement,
    RawMessage, Timestamp,
};
use sandesh_storage::{
    CollectingSink, Database, EventRecorder, FilesystemBlobStore, INLINE_THRESHOLD,
    SqliteMessageStore,
};
use std::sync::Arc;

#[tokio::test]
async fn every_fixture_round_trips_byte_for_byte() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for (name, original) in common::all_fixtures() {
        let inserted = h.deliver(&original, &[inbox]).await;

        let stored = h
            .messages
            .raw(inserted.message_id)
            .await
            .expect("query raw")
            .expect("raw exists");

        assert_eq!(
            stored.bytes(),
            original.as_slice(),
            "{name}: stored bytes differ from what was received"
        );
        assert_eq!(stored.size(), original.len() as u64, "{name}: RFC822.SIZE");
        assert!(stored.verify_integrity(), "{name}: digest mismatch");
    }
}

#[tokio::test]
async fn line_endings_are_never_normalised() {
    // A client sending bare LF must get bare LF back. Normalising would destroy
    // exactly the fidelity that justifies the product.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let bare_lf = common::fixture("malformed/bare-lf.eml");
    assert!(
        !bare_lf.windows(2).any(|w| w == b"\r\n"),
        "fixture must genuinely use bare LF"
    );

    let inserted = h.deliver(&bare_lf, &[inbox]).await;
    let stored = h
        .messages
        .raw(inserted.message_id)
        .await
        .expect("query")
        .expect("exists");

    assert_eq!(stored.bytes(), bare_lf.as_slice());
    assert!(
        !stored.bytes().windows(2).any(|w| w == b"\r\n"),
        "bare LF must not be rewritten to CRLF"
    );
}

#[tokio::test]
async fn malformed_messages_are_stored_and_retrievable() {
    // A parse failure must never lose mail: storing malformed messages is the
    // entire point of the tool.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for (name, original) in common::all_fixtures()
        .into_iter()
        .filter(|(n, _)| n.starts_with("malformed/"))
    {
        let inserted = h.deliver(&original, &[inbox]).await;
        let stored = h
            .messages
            .raw(inserted.message_id)
            .await
            .expect("query")
            .expect("exists");

        assert_eq!(stored.bytes(), original.as_slice(), "{name}");
        assert_eq!(inserted.entries.len(), 1, "{name}: must still be delivered");
    }
}

#[tokio::test]
async fn binary_and_eight_bit_content_survives() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    // Every byte value 0..=255 in the body, including NUL and invalid UTF-8.
    let mut raw = b"Subject: binary\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    raw.extend(0u8..=255);

    let inserted = h.deliver(&raw, &[inbox]).await;
    let stored = h
        .messages
        .raw(inserted.message_id)
        .await
        .expect("query")
        .expect("exists");

    assert_eq!(
        stored.bytes(),
        raw.as_slice(),
        "binary content must be exact"
    );
}

#[tokio::test]
async fn mime_part_ranges_slice_the_original_bytes_exactly() {
    // The property IMAP FETCH BODY[2.1] depends on.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let raw = common::fixture("valid/multipart-alternative.eml");
    let inserted = h.deliver(&raw, &[inbox]).await;
    let message = h.get_message(inserted.message_id).await;

    let plain = message
        .parsed
        .parts
        .iter()
        .find(|p| p.content_type == "text/plain")
        .expect("a text/plain part");

    let body = plain.body(message.raw.bytes()).expect("body in bounds");
    // Note the absent trailing CRLF: RFC 2046 §5.1.1 attaches the CRLF that
    // precedes a boundary delimiter to the delimiter, not to the part body.
    // This is the boundary semantics IMAP FETCH BODY[<part>] must reproduce.
    assert_eq!(
        body, b"Plain version of the message.",
        "the part body must be the original bytes, not a re-serialisation"
    );

    // The bytes must be a genuine slice of the stored message, at the recorded
    // offset — not an equal-looking copy produced some other way.
    assert_eq!(
        &message.raw.bytes()[plain.body_range.start()..plain.body_range.end()],
        body
    );

    // Every range must stay inside the stored message.
    for part in &message.parsed.parts {
        assert!(part.body_range.is_within(message.raw.bytes().len()));
        assert!(part.header_range.is_within(message.raw.bytes().len()));
    }
}

#[tokio::test]
async fn oversized_messages_round_trip_through_the_blob_store() {
    // Exercises the other half of the hybrid storage decision (ADR-007).
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open_in_memory().await.expect("open db");
    let blobs = Arc::new(
        FilesystemBlobStore::new(dir.path())
            .await
            .expect("blob store"),
    );
    let sink = Arc::new(CollectingSink::new());
    let events = EventRecorder::new(sink);

    let accounts = sandesh_storage::SqliteAccountStore::new(db.pool().clone(), events.clone());
    let mailboxes = sandesh_storage::SqliteMailboxStore::new(db.pool().clone(), events.clone());
    let messages = SqliteMessageStore::new(db.pool().clone(), blobs.clone(), events);

    use sandesh_core::{Account, AccountId, AccountStore, MailboxName, MailboxStore};
    let account = Account {
        id: AccountId::new(),
        username: String::from("dev"),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    };
    accounts.create(&account).await.expect("create account");
    let inbox = mailboxes
        .get_by_name(account.id, &MailboxName::new("INBOX").expect("name"))
        .await
        .expect("query")
        .expect("inbox")
        .id;

    // Comfortably above the inline threshold.
    let mut raw = b"Subject: large\r\nContent-Type: text/plain\r\n\r\n".to_vec();
    raw.extend(std::iter::repeat_n(b'A', INLINE_THRESHOLD + 4096));

    let now = Timestamp::now();
    let raw_message = RawMessage::new(raw.clone());
    let parsed = sandesh_core::parse(raw_message.bytes());
    let inserted = messages
        .insert(&NewMessage {
            raw: raw_message,
            envelope: Envelope::default(),
            source: DeliverySource::Smtp,
            parsed,
            received_at: now,
            placements: vec![NewPlacement {
                mailbox_id: inbox,
                flags: MessageFlags::empty(),
                internal_date: now,
            }],
        })
        .await
        .expect("insert");

    // Stored externally, not inline.
    let (inline, blob_ref): (Option<Vec<u8>>, Option<String>) =
        sqlx::query_as("SELECT raw_inline, raw_blob_ref FROM messages WHERE id = ?")
            .bind(&inserted.message_id.as_bytes()[..])
            .fetch_one(db.pool())
            .await
            .expect("query storage location");
    assert!(inline.is_none(), "large messages must not be stored inline");
    let blob_ref = blob_ref.expect("blob reference recorded");

    let stored = messages
        .raw(inserted.message_id)
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(
        stored.bytes(),
        raw.as_slice(),
        "blob round-trip must be exact"
    );

    // Collecting the message removes the object too.
    messages
        .delete_entry(inserted.entries[0].id)
        .await
        .expect("delete entry");
    messages.collect_orphans().await.expect("gc");
    assert!(
        blobs.get(&blob_ref).await.expect("blob get").is_none(),
        "the blob must be reclaimed with its message"
    );
}

#[tokio::test]
async fn small_messages_stay_inline() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    let (inline, blob_ref): (Option<Vec<u8>>, Option<String>) =
        sqlx::query_as("SELECT raw_inline, raw_blob_ref FROM messages WHERE id = ?")
            .bind(&inserted.message_id.as_bytes()[..])
            .fetch_one(h.db.pool())
            .await
            .expect("query");

    assert!(inline.is_some(), "small messages stay in the row");
    assert!(blob_ref.is_none(), "and carry no blob reference");
}

#[tokio::test]
async fn the_corpus_meets_the_size_and_shape_the_strategy_requires() {
    // `TESTING_STRATEGY.md` §4 and `ROADMAP.md` Phase 1 require ≥100 `.eml`
    // fixtures across five named categories. That number is not decoration:
    // several acceptance criteria are phrased "for every fixture in the
    // corpus", so the corpus size *is* the strength of those criteria, and a
    // corpus that quietly shrinks weakens them without any test failing.
    //
    // This asserts the contract directly. It is also what makes
    // `common::CATEGORIES` load-bearing: a category dropped from that list
    // stops being walked by every corpus test, and this test is what notices.
    let fixtures = common::all_fixtures();

    assert!(
        fixtures.len() >= 100,
        "the corpus has {} fixtures; TESTING_STRATEGY §4 requires at least 100",
        fixtures.len()
    );

    for category in common::CATEGORIES {
        let prefix = format!("{category}/");
        let count = fixtures
            .iter()
            .filter(|(name, _)| name.starts_with(&prefix))
            .count();
        assert!(
            count > 0,
            "category `{category}` has no fixtures; every category in \
             TESTING_STRATEGY §4 must be represented"
        );
    }

    // Every fixture must be readable and non-trivial to load. `malformed/`
    // deliberately includes an empty file, which is a legitimate fixture and
    // the reason this is not a blanket non-empty assertion.
    let empty: Vec<&String> = fixtures
        .iter()
        .filter(|(name, bytes)| bytes.is_empty() && !name.starts_with("malformed/"))
        .map(|(name, _)| name)
        .collect();
    assert!(
        empty.is_empty(),
        "only `malformed/` may contain empty fixtures; found {empty:?}"
    );
}
