//! Database-enforced invariants, transaction atomicity, and concurrency.
//!
//! These invariants are enforced by the schema rather than by Rust alone: a
//! constraint that lives only in application code is unguarded against
//! concurrent writers, and UID uniqueness is exactly the case where a race
//! would corrupt every connected client's cache.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    DeliverySource, Envelope, MailboxId, MessageFlags, MessageStore, NewMessage, NewPlacement,
    RawMessage, Timestamp,
};
use sandesh_storage::Database;

#[tokio::test]
async fn foreign_keys_reject_orphan_rows() {
    let h = Harness::new().await;

    let orphan = sqlx::query(
        "INSERT INTO message_entries
            (id, message_id, mailbox_id, uid, flags, internal_date, added_at, mod_seq)
         VALUES (?, ?, ?, 1, 0, 0, 0, 1)",
    )
    .bind(&sandesh_core::EntryId::new().as_bytes()[..])
    .bind(&sandesh_core::MessageId::new().as_bytes()[..])
    .bind(&MailboxId::new().as_bytes()[..])
    .execute(h.db.pool())
    .await;

    assert!(orphan.is_err(), "a placement must reference a real message");
}

#[tokio::test]
async fn uid_zero_is_rejected_by_the_schema() {
    // RFC 9051 reserves UID 0. The domain type also refuses it; this checks the
    // second line of defence.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    let bad = sqlx::query("UPDATE message_entries SET uid = 0 WHERE id = ?")
        .bind(&inserted.entries[0].id.as_bytes()[..])
        .execute(h.db.pool())
        .await;
    assert!(bad.is_err(), "UID 0 must be rejected");
}

#[tokio::test]
async fn duplicate_uid_in_a_mailbox_is_rejected() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    h.deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let second = h
        .deliver(&common::fixture("valid/html.eml"), &[inbox])
        .await;

    let clash = sqlx::query("UPDATE message_entries SET uid = 1 WHERE id = ?")
        .bind(&second.entries[0].id.as_bytes()[..])
        .execute(h.db.pool())
        .await;
    assert!(clash.is_err(), "UIDs must be unique within a mailbox");
}

#[tokio::test]
async fn raw_bytes_must_live_in_exactly_one_place() {
    // The CHECK that keeps inline and blob storage mutually exclusive.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    let both = sqlx::query("UPDATE messages SET raw_blob_ref = 'ref' WHERE id = ?")
        .bind(&inserted.message_id.as_bytes()[..])
        .execute(h.db.pool())
        .await;
    assert!(both.is_err(), "a message cannot be stored in two places");

    let neither = sqlx::query("UPDATE messages SET raw_inline = NULL WHERE id = ?")
        .bind(&inserted.message_id.as_bytes()[..])
        .execute(h.db.pool())
        .await;
    assert!(neither.is_err(), "a message cannot be stored nowhere");
}

#[tokio::test]
async fn only_one_catch_all_account_is_permitted() {
    let h = Harness::new().await;

    for username in ["first", "second"] {
        let account = sandesh_core::Account {
            id: sandesh_core::AccountId::new(),
            username: username.to_owned(),
            credential: None,
            is_catch_all: true,
            created_at: Timestamp::now(),
        };
        let result = sandesh_core::AccountStore::create(&h.accounts, &account).await;
        if username == "second" {
            assert!(result.is_err(), "a second catch-all must be rejected");
        } else {
            result.expect("the first catch-all is fine");
        }
    }

    assert!(
        sandesh_core::AccountStore::catch_all(&h.accounts)
            .await
            .expect("query")
            .is_some()
    );
}

#[tokio::test]
async fn delivery_is_atomic_across_all_placements() {
    // If any placement fails, nothing is persisted (CORE_SPEC §7.5).
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(h.db.pool())
        .await
        .expect("count");

    let raw = RawMessage::new(common::fixture("valid/plain-text.eml"));
    let parsed = sandesh_core::parse(raw.bytes());
    let now = Timestamp::now();

    // The second placement targets a mailbox that does not exist.
    let result = h
        .messages
        .insert(&NewMessage {
            raw,
            envelope: Envelope::default(),
            source: DeliverySource::Smtp,
            parsed,
            received_at: now,
            placements: vec![
                NewPlacement {
                    mailbox_id: inbox,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                },
                NewPlacement {
                    mailbox_id: MailboxId::new(),
                    flags: MessageFlags::empty(),
                    internal_date: now,
                },
            ],
        })
        .await;

    assert!(result.is_err(), "the delivery must fail");

    let after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(h.db.pool())
        .await
        .expect("count");
    assert_eq!(after.0, before.0, "no message row may survive a rollback");

    let entries: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM message_entries")
        .fetch_one(h.db.pool())
        .await
        .expect("count");
    assert_eq!(entries.0, 0, "no placement may survive a rollback");

    // The mailbox's UID counter must not have advanced either.
    let counts = sandesh_core::MailboxStore::counts(&h.mailboxes, inbox)
        .await
        .expect("counts");
    assert_eq!(counts.uid_next, 1, "a rolled-back delivery consumes no UID");
}

#[tokio::test]
async fn concurrent_delivery_assigns_distinct_contiguous_uids() {
    // The one race that would corrupt client state. Uses a file-backed database
    // so several connections genuinely contend.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(dir.path().join("concurrent.db"))
        .await
        .expect("open");

    let events = sandesh_storage::EventRecorder::null();
    let accounts = sandesh_storage::SqliteAccountStore::new(db.pool().clone(), events.clone());
    let mailboxes = sandesh_storage::SqliteMailboxStore::new(db.pool().clone(), events.clone());
    let messages = std::sync::Arc::new(sandesh_storage::SqliteMessageStore::new(
        db.pool().clone(),
        std::sync::Arc::new(sandesh_storage::NullBlobStore),
        events,
    ));

    let account = sandesh_core::Account {
        id: sandesh_core::AccountId::new(),
        username: String::from("dev"),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    };
    sandesh_core::AccountStore::create(&accounts, &account)
        .await
        .expect("create account");
    let inbox = sandesh_core::MailboxStore::get_by_name(
        &mailboxes,
        account.id,
        &sandesh_core::MailboxName::new("INBOX").expect("name"),
    )
    .await
    .expect("query")
    .expect("inbox")
    .id;

    const DELIVERIES: usize = 40;
    let mut tasks = Vec::with_capacity(DELIVERIES);
    for n in 0..DELIVERIES {
        let messages = messages.clone();
        tasks.push(tokio::spawn(async move {
            let raw = RawMessage::new(
                format!("Subject: concurrent {n}\r\n\r\nbody {n}\r\n").into_bytes(),
            );
            let parsed = sandesh_core::parse(raw.bytes());
            let now = Timestamp::now();
            messages
                .insert(&NewMessage {
                    raw,
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
        }));
    }

    let mut uids = Vec::with_capacity(DELIVERIES);
    for task in tasks {
        let inserted = task.await.expect("task").expect("insert");
        uids.push(inserted.entries[0].uid.get());
    }

    uids.sort_unstable();
    assert_eq!(
        uids,
        (1..=DELIVERIES as u32).collect::<Vec<_>>(),
        "concurrent deliveries must produce distinct, contiguous UIDs"
    );
}

#[tokio::test]
async fn uid_next_never_decreases_after_deletion() {
    // Reusing a UID would corrupt every client's cache.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for _ in 0..3 {
        h.deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
    }
    let entries = h.messages.list_entries(inbox).await.expect("list");
    for entry in &entries {
        h.messages.delete_entry(entry.id).await.expect("delete");
    }

    let counts = sandesh_core::MailboxStore::counts(&h.mailboxes, inbox)
        .await
        .expect("counts");
    assert_eq!(counts.total, 0);
    assert_eq!(counts.uid_next, 4, "uid_next must not rewind");

    // The next delivery continues the sequence rather than reusing UID 1.
    let next = h
        .deliver(&common::fixture("valid/html.eml"), &[inbox])
        .await;
    assert_eq!(next.entries[0].uid.get(), 4);
}

#[tokio::test]
async fn a_file_backed_database_survives_reopening() {
    // Migrations must be idempotent and data must persist across restarts.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("persist.db");

    let message_id = {
        let db = Database::open(&path).await.expect("open");
        let events = sandesh_storage::EventRecorder::null();
        let accounts = sandesh_storage::SqliteAccountStore::new(db.pool().clone(), events.clone());
        let mailboxes = sandesh_storage::SqliteMailboxStore::new(db.pool().clone(), events.clone());
        let messages = sandesh_storage::SqliteMessageStore::new(
            db.pool().clone(),
            std::sync::Arc::new(sandesh_storage::NullBlobStore),
            events,
        );

        let account = sandesh_core::Account {
            id: sandesh_core::AccountId::new(),
            username: String::from("dev"),
            credential: None,
            is_catch_all: false,
            created_at: Timestamp::now(),
        };
        sandesh_core::AccountStore::create(&accounts, &account)
            .await
            .expect("create");
        let inbox = sandesh_core::MailboxStore::get_by_name(
            &mailboxes,
            account.id,
            &sandesh_core::MailboxName::new("INBOX").expect("name"),
        )
        .await
        .expect("query")
        .expect("inbox")
        .id;

        let raw = RawMessage::new(common::fixture("valid/plain-text.eml"));
        let parsed = sandesh_core::parse(raw.bytes());
        let now = Timestamp::now();
        let inserted = messages
            .insert(&NewMessage {
                raw,
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

        db.close().await;
        inserted.message_id
    };

    // Reopen: migrations re-run without error and the message is still there.
    let db = Database::open(&path).await.expect("reopen");
    let messages = sandesh_storage::SqliteMessageStore::new(
        db.pool().clone(),
        std::sync::Arc::new(sandesh_storage::NullBlobStore),
        sandesh_storage::EventRecorder::null(),
    );

    let stored = messages
        .raw(message_id)
        .await
        .expect("query")
        .expect("message survived the restart");
    assert_eq!(
        stored.bytes(),
        common::fixture("valid/plain-text.eml").as_slice()
    );
}
