//! `page_entries` in SQL against the reference scan.
//!
//! The message list used to group, sort and truncate every placement in Rust on
//! each page. `SqliteMessageStore` now answers in SQL; this pins that it gives
//! exactly what the reference (`page_entries_by_scan`, the trait default)
//! gives, for every page of a full cursor walk, over placements that exercise
//! what the grouping is for: one message in several mailboxes, mailboxes with
//! independent UID sequences, and scopes that leave some mailboxes out.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{EntryPage, MailboxId, MessageStore, page_entries_by_scan};

fn message(n: usize) -> Vec<u8> {
    format!("From: a@example.test\r\nTo: b@example.test\r\nSubject: m{n}\r\n\r\nbody {n}\r\n")
        .into_bytes()
}

/// Walks every page the way the API does — the next cursor is the lowest UID on
/// the page — asserting each SQL page equals the reference page.
async fn assert_parity(harness: &Harness, scope: &[MailboxId], limit: usize) -> usize {
    let mut cursor = None;
    let mut seen = 0;
    loop {
        let sql = harness
            .messages
            .page_entries(scope, cursor, limit)
            .await
            .expect("sql page");
        let reference = page_entries_by_scan(&harness.messages, scope, cursor, limit)
            .await
            .expect("reference page");
        assert_eq!(
            sql, reference,
            "scope {scope:?}, cursor {cursor:?}, limit {limit}"
        );

        seen += sql.messages.len();
        if !sql.has_more {
            return seen;
        }
        cursor = lowest_uid(&sql);
    }
}

fn lowest_uid(page: &EntryPage) -> Option<u32> {
    page.messages
        .last()
        .and_then(|(_, entries)| entries.iter().map(|entry| entry.uid.get()).min())
}

#[tokio::test]
async fn sql_pages_match_the_reference_scan() {
    let harness = Harness::new().await;
    let account = harness.account("dev", "dev@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let archive = harness.mailbox(account, "Archive").await;
    let other = harness.account("ops", "ops@example.test").await;
    let ops_inbox = harness.mailbox(other, "INBOX").await;

    for n in 0..37 {
        let placements: &[MailboxId] = match n % 5 {
            0 => &[inbox, archive],
            1 | 2 => &[inbox],
            3 => &[ops_inbox],
            _ => &[archive, ops_inbox],
        };
        let inserted = harness.deliver(&message(n), placements).await;
        // Some messages gain a later placement, so their newest `added_at`
        // differs from their first one's.
        if n % 7 == 0 && !placements.contains(&ops_inbox) {
            harness
                .messages
                .copy_entry(inserted.entries[0].id, ops_inbox)
                .await
                .expect("copy");
        }
    }

    let everything = [inbox, archive, ops_inbox];
    for limit in [1, 4, 10, 50] {
        assert_parity(&harness, &everything, limit).await;
        assert_parity(&harness, &[inbox], limit).await;
        assert_parity(&harness, &[archive, ops_inbox], limit).await;
    }
    assert_eq!(
        assert_parity(&harness, &[inbox], 1000).await,
        23,
        "every INBOX message"
    );
}

#[tokio::test]
async fn an_empty_or_unknown_scope_is_an_empty_page() {
    let harness = Harness::new().await;
    let account = harness.account("dev", "dev@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    harness.deliver(&message(0), &[inbox]).await;

    for scope in [vec![], vec![MailboxId::new()]] {
        let page = harness
            .messages
            .page_entries(&scope, None, 10)
            .await
            .expect("page");
        assert_eq!(page, EntryPage::default(), "{scope:?}");
    }
}
