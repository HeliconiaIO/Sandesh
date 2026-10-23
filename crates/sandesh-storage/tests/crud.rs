//! CRUD and cascade behaviour for accounts, mailboxes and placements.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    Account, AccountId, AccountStore, CoreError, EmailAddress, Mailbox, MailboxAttributes,
    MailboxId, MailboxName, MailboxStore, MessageStore, Timestamp, Uid, UidValidity,
};

fn account_named(username: &str) -> Account {
    Account {
        id: AccountId::new(),
        username: username.to_owned(),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    }
}

#[tokio::test]
async fn accounts_round_trip() {
    let h = Harness::new().await;
    let account = account_named("dev");
    h.accounts.create(&account).await.expect("create");

    let by_id = h.accounts.get(account.id).await.expect("query");
    let by_name = h.accounts.get_by_username("dev").await.expect("query");
    assert_eq!(by_id.map(|a| a.id), Some(account.id));
    assert_eq!(by_name.map(|a| a.id), Some(account.id));

    assert_eq!(h.accounts.list().await.expect("list").len(), 1);
}

#[tokio::test]
async fn duplicate_username_is_rejected() {
    let h = Harness::new().await;
    h.accounts
        .create(&account_named("dev"))
        .await
        .expect("first");

    let second = h.accounts.create(&account_named("dev")).await;
    assert!(
        matches!(second, Err(CoreError::AlreadyExists { .. })),
        "got {second:?}"
    );
}

#[tokio::test]
async fn one_address_cannot_route_to_two_accounts() {
    // Routing must be deterministic; the database enforces it.
    let h = Harness::new().await;
    let a = account_named("a");
    let b = account_named("b");
    h.accounts.create(&a).await.expect("create a");
    h.accounts.create(&b).await.expect("create b");

    let address = EmailAddress::parse_lenient("shared@example.test");
    h.accounts
        .add_identity(a.id, &address, true)
        .await
        .expect("first identity");

    let clash = h.accounts.add_identity(b.id, &address, true).await;
    assert!(
        matches!(clash, Err(CoreError::AlreadyExists { .. })),
        "got {clash:?}"
    );
}

#[tokio::test]
async fn address_resolution_folds_domain_case() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@Example.TEST").await;

    for spelling in ["dev@example.test", "dev@EXAMPLE.TEST", "dev@Example.Test"] {
        let resolved = h
            .accounts
            .resolve_address(&EmailAddress::parse_lenient(spelling))
            .await
            .expect("resolve");
        assert_eq!(resolved, Some(account), "{spelling} must resolve");
    }

    // The local part is case-sensitive (RFC 5321 §2.3.11).
    let different = h
        .accounts
        .resolve_address(&EmailAddress::parse_lenient("DEV@example.test"))
        .await
        .expect("resolve");
    assert_eq!(different, None);
}

#[tokio::test]
async fn deleting_an_account_cascades_to_everything_it_owns() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    h.deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    assert!(h.accounts.delete(account).await.expect("delete"));

    assert!(h.accounts.get(account).await.expect("query").is_none());
    assert!(h.mailboxes.list(account).await.expect("list").is_empty());
    assert!(
        h.accounts
            .identities(account)
            .await
            .expect("query")
            .is_empty()
    );

    let entries: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM message_entries")
        .fetch_one(h.db.pool())
        .await
        .expect("count");
    assert_eq!(entries.0, 0, "placements go with their mailboxes");

    // The message itself is left for the orphan sweep rather than assumed dead.
    let messages: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(h.db.pool())
        .await
        .expect("count");
    assert_eq!(messages.0, 1, "deletion is not assumed transitive");
    assert_eq!(h.messages.collect_orphans().await.expect("gc").len(), 1);
}

#[tokio::test]
async fn deleting_a_missing_entity_reports_false_rather_than_erroring() {
    let h = Harness::new().await;
    assert!(!h.accounts.delete(AccountId::new()).await.expect("delete"));
    assert!(!h.mailboxes.delete(MailboxId::new()).await.expect("delete"));
    assert!(
        !h.messages
            .delete_entry(sandesh_core::EntryId::new())
            .await
            .expect("delete")
    );
}

#[tokio::test]
async fn every_account_gets_the_default_mailboxes() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;

    let names: Vec<String> = h
        .mailboxes
        .list(account)
        .await
        .expect("list")
        .iter()
        .map(|m| m.name.as_str().to_owned())
        .collect();

    for expected in ["INBOX", "Sent", "Drafts", "Trash", "Junk", "Archive"] {
        assert!(names.contains(&expected.to_owned()), "missing {expected}");
    }
}

#[tokio::test]
async fn creating_a_nested_mailbox_creates_its_parents() {
    // IMAP CREATE requires implicit parent creation (CORE_SPEC §6).
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;

    let deep = Mailbox {
        id: MailboxId::new(),
        account_id: account,
        name: MailboxName::new("Work/Projects/Sandesh").expect("name"),
        parent_id: None,
        uid_validity: UidValidity::new(1),
        uid_next: Uid::MIN,
        attributes: MailboxAttributes::none(),
        subscribed: true,
        created_at: Timestamp::now(),
        highest_mod_seq: sandesh_core::ModSeq::MIN,
    };
    h.mailboxes.create(&deep).await.expect("create nested");

    let names: Vec<String> = h
        .mailboxes
        .list(account)
        .await
        .expect("list")
        .iter()
        .map(|m| m.name.as_str().to_owned())
        .collect();

    assert!(names.contains(&String::from("Work")), "got {names:?}");
    assert!(names.contains(&String::from("Work/Projects")));
    assert!(names.contains(&String::from("Work/Projects/Sandesh")));

    // Parent links are wired, so a future LIST can walk the hierarchy.
    let created = h
        .mailboxes
        .get(deep.id)
        .await
        .expect("query")
        .expect("exists");
    assert!(
        created.parent_id.is_some(),
        "the child must know its parent"
    );
}

#[tokio::test]
async fn renaming_a_mailbox_moves_its_descendants() {
    // RFC 9051: descendants follow the parent.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;

    for name in ["Work", "Work/Projects", "Work/Projects/Sandesh", "Workshop"] {
        h.mailboxes
            .create(&Mailbox {
                id: MailboxId::new(),
                account_id: account,
                name: MailboxName::new(name).expect("name"),
                parent_id: None,
                uid_validity: UidValidity::new(1),
                uid_next: Uid::MIN,
                attributes: MailboxAttributes::none(),
                subscribed: true,
                created_at: Timestamp::now(),
                highest_mod_seq: sandesh_core::ModSeq::MIN,
            })
            .await
            .expect("create");
    }

    let work = h.mailbox(account, "Work").await;
    h.mailboxes
        .rename(work, &MailboxName::new("Archive2026").expect("name"))
        .await
        .expect("rename");

    let names: Vec<String> = h
        .mailboxes
        .list(account)
        .await
        .expect("list")
        .iter()
        .map(|m| m.name.as_str().to_owned())
        .collect();

    assert!(names.contains(&String::from("Archive2026")));
    assert!(names.contains(&String::from("Archive2026/Projects")));
    assert!(names.contains(&String::from("Archive2026/Projects/Sandesh")));
    assert!(
        names.contains(&String::from("Workshop")),
        "a sibling with a shared prefix must not be dragged along: {names:?}"
    );
}

#[tokio::test]
async fn renaming_preserves_uids() {
    // A rename does not invalidate client caches, so UIDs must survive it.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[archive])
        .await;
    let uid_before = inserted.entries[0].uid;
    let validity_before = h
        .mailboxes
        .get(archive)
        .await
        .expect("query")
        .expect("exists")
        .uid_validity;

    h.mailboxes
        .rename(archive, &MailboxName::new("Archived").expect("name"))
        .await
        .expect("rename");

    let entries = h.messages.list_entries(archive).await.expect("list");
    assert_eq!(entries[0].uid, uid_before, "UIDs survive a rename");

    let after = h
        .mailboxes
        .get(archive)
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(
        after.uid_validity, validity_before,
        "UIDVALIDITY is unchanged"
    );
}

#[tokio::test]
async fn subscription_state_can_be_toggled() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let junk = h.mailbox(account, "Junk").await;

    h.mailboxes
        .set_subscribed(junk, false)
        .await
        .expect("unsubscribe");
    let mailbox = h.mailboxes.get(junk).await.expect("query").expect("exists");
    assert!(!mailbox.subscribed);

    h.mailboxes
        .set_subscribed(junk, true)
        .await
        .expect("subscribe");
    let mailbox = h.mailboxes.get(junk).await.expect("query").expect("exists");
    assert!(mailbox.subscribed);
}

#[tokio::test]
async fn mailbox_counts_track_placements_and_unseen() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for _ in 0..3 {
        h.deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
    }

    let counts = h.mailboxes.counts(inbox).await.expect("counts");
    assert_eq!(counts.total, 3);
    assert_eq!(counts.unseen, 3, "delivered messages start unread");
    assert_eq!(counts.uid_next, 4);

    // Marking one seen reduces the unseen count.
    let entries = h.messages.list_entries(inbox).await.expect("list");
    h.messages
        .update_flags(
            entries[0].id,
            sandesh_core::FlagUpdate::Add {
                system: sandesh_core::SystemFlags::SEEN,
                keywords: std::collections::BTreeSet::new(),
            },
        )
        .await
        .expect("flag");

    assert_eq!(h.mailboxes.counts(inbox).await.expect("counts").unseen, 2);
}

#[tokio::test]
async fn deleting_a_mailbox_removes_its_placements_only() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    h.messages
        .copy_entry(inserted.entries[0].id, archive)
        .await
        .expect("copy");

    assert!(h.mailboxes.delete(inbox).await.expect("delete"));

    assert!(h.mailboxes.get(inbox).await.expect("query").is_none());
    assert_eq!(
        h.messages
            .entries_for_message(inserted.message_id)
            .await
            .expect("query")
            .len(),
        1,
        "the Archive placement survives"
    );
    assert!(
        h.messages
            .get(inserted.message_id)
            .await
            .expect("query")
            .is_some(),
        "the message survives while a placement remains"
    );
}

#[tokio::test]
async fn entries_are_listed_in_uid_order() {
    // IMAP relies on this ordering.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for _ in 0..10 {
        h.deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
    }

    let entries = h.messages.list_entries(inbox).await.expect("list");
    let uids: Vec<u32> = entries.iter().map(|e| e.uid.get()).collect();
    assert_eq!(uids, (1..=10).collect::<Vec<_>>());
}
