//! The primary Phase 1 acceptance test.
//!
//! Walks the whole path an SMTP delivery will take in Phase 2 — account,
//! identity, mailbox, message, raw bytes, placement, UID, flags — and verifies
//! every layer after a round trip through SQLite.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    Account, AccountId, AccountStore, DeliverySource, EmailAddress, Envelope, FlagUpdate, Keyword,
    MailboxName, MailboxStore, MessageFlags, MessageStore, NewMessage, NewPlacement, RawMessage,
    SystemFlags, Timestamp, Uid,
};
use std::collections::BTreeSet;

#[tokio::test]
async fn full_lifecycle_from_account_to_verified_retrieval() {
    let h = Harness::new().await;

    // ---- account -------------------------------------------------------
    let account = Account {
        id: AccountId::new(),
        username: String::from("dev"),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    };
    h.accounts.create(&account).await.expect("create account");

    let fetched = h
        .accounts
        .get(account.id)
        .await
        .expect("query")
        .expect("account exists");
    assert_eq!(fetched.username, "dev");

    // ---- identity ------------------------------------------------------
    let address = EmailAddress::parse_lenient("Dev Team <dev@Example.TEST>");
    h.accounts
        .add_identity(account.id, &address, true)
        .await
        .expect("add identity");

    // Routing must be case-insensitive on the domain.
    let resolved = h
        .accounts
        .resolve_address(&EmailAddress::parse_lenient("dev@example.test"))
        .await
        .expect("resolve");
    assert_eq!(resolved, Some(account.id));

    let identities = h.accounts.identities(account.id).await.expect("identities");
    assert_eq!(identities.len(), 1);
    assert_eq!(
        identities[0].address.original(),
        "Dev Team <dev@Example.TEST>",
        "the address as supplied must be preserved"
    );

    // ---- mailbox -------------------------------------------------------
    // INBOX is created with the account; IMAP and POP3 both require it.
    let inbox = h
        .mailboxes
        .get_by_name(account.id, &MailboxName::new("INBOX").expect("name"))
        .await
        .expect("query")
        .expect("INBOX exists");
    assert_eq!(inbox.uid_next, Uid::MIN, "a fresh mailbox starts at UID 1");

    let all = h.mailboxes.list(account.id).await.expect("list");
    assert_eq!(all.len(), 6, "INBOX plus the five SPECIAL-USE mailboxes");
    assert!(
        all.iter().any(|m| m.attributes.contains(r"\Sent")),
        "SPECIAL-USE attributes must be set"
    );

    // ---- message -------------------------------------------------------
    let original = common::fixture("valid/multipart-mixed.eml");
    let raw = RawMessage::new(original.clone());
    let expected_digest = *raw.sha256();
    let parsed = sandesh_core::parse(raw.bytes());
    let received_at = Timestamp::now();

    let mut initial_flags = MessageFlags::from_system(SystemFlags::SEEN);
    initial_flags.add(
        SystemFlags::NONE,
        [Keyword::new("$Fixture").expect("keyword")],
    );

    let inserted = h
        .messages
        .insert(&NewMessage {
            raw,
            envelope: Envelope {
                mail_from: Some(EmailAddress::parse_lenient("alice@example.test")),
                rcpt_to: vec![address.clone()],
                helo_domain: Some(String::from("client.test")),
                peer: Some(String::from("127.0.0.1:54321")),
                tls: None,
                auth_identity: None,
            },
            source: DeliverySource::Smtp,
            parsed,
            received_at,
            placements: vec![NewPlacement {
                mailbox_id: inbox.id,
                flags: initial_flags.clone(),
                internal_date: received_at,
            }],
        })
        .await
        .expect("insert message");

    // ---- verify the placement -----------------------------------------
    assert_eq!(inserted.entries.len(), 1);
    let entry = &inserted.entries[0];
    assert_eq!(entry.uid, Uid::MIN, "the first placement gets UID 1");
    assert!(entry.flags.has(SystemFlags::SEEN));
    assert!(
        entry
            .flags
            .has_keyword(&Keyword::new("$Fixture").expect("kw"))
    );

    // The mailbox counter advanced.
    let counts = h.mailboxes.counts(inbox.id).await.expect("counts");
    assert_eq!(counts.total, 1);
    assert_eq!(counts.unseen, 0, "the message was delivered \\Seen");
    assert_eq!(counts.uid_next, 2, "uid_next advanced past the assignment");

    // ---- verify raw bytes ---------------------------------------------
    let stored_raw = h
        .messages
        .raw(inserted.message_id)
        .await
        .expect("query raw")
        .expect("raw exists");
    assert_eq!(
        stored_raw.bytes(),
        original.as_slice(),
        "raw bytes must survive persistence exactly"
    );
    assert_eq!(stored_raw.sha256(), &expected_digest);
    assert!(stored_raw.verify_integrity());

    // ---- verify metadata ----------------------------------------------
    let message = h.get_message(inserted.message_id).await;
    assert_eq!(message.size(), original.len() as u64);
    assert_eq!(message.source, DeliverySource::Smtp);
    assert_eq!(message.subject(), Some("Mixed parts"));
    // The decoded value is the msg-id content; the angle brackets are
    // delimiters, and remain in the raw header bytes.
    assert_eq!(message.message_id_header(), Some("mixed-004@example.test"));
    let raw_msgid = message
        .parsed
        .header("message-id")
        .expect("Message-ID header");
    assert!(
        raw_msgid.raw_value.windows(1).any(|b| b == b"<"),
        "the raw header bytes keep the delimiters"
    );
    assert_eq!(
        message
            .envelope
            .mail_from
            .as_ref()
            .map(EmailAddress::normalized),
        Some(String::from("alice@example.test")),
        "the SMTP envelope is preserved separately from the headers"
    );
    assert_eq!(message.envelope.helo_domain.as_deref(), Some("client.test"));

    // Headers survive in order, with duplicates.
    assert!(
        message.parsed.headers.len() >= 6,
        "all headers persisted, got {}",
        message.parsed.headers.len()
    );

    // ---- verify MIME ranges -------------------------------------------
    assert!(
        message.parsed.parts.len() >= 3,
        "multipart/mixed root plus two children"
    );
    for part in &message.parsed.parts {
        assert!(
            part.body_range.is_within(message.raw.bytes().len()),
            "part {} body range escapes the message",
            part.path
        );
        assert!(part.header_range.is_within(message.raw.bytes().len()));
    }

    // Byte ranges still address the right content after the round trip.
    let attachment_part = message
        .parsed
        .parts
        .iter()
        .find(|p| p.filename.as_deref() == Some("notes.txt"))
        .expect("the attachment part");
    let body = attachment_part
        .body(message.raw.bytes())
        .expect("body in bounds");
    assert_eq!(body, b"Attached plain text file.");

    // ---- verify attachments -------------------------------------------
    let attachments = h
        .messages
        .attachments(inserted.message_id)
        .await
        .expect("attachments");
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].filename.as_deref(), Some("notes.txt"));
    assert!(!attachments[0].is_inline);

    // ---- verify flag mutation ------------------------------------------
    let updated = h
        .messages
        .update_flags(
            entry.id,
            FlagUpdate::Add {
                system: SystemFlags::FLAGGED,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("update flags");
    assert!(updated.has(SystemFlags::FLAGGED));
    assert!(
        updated.has(SystemFlags::SEEN),
        "existing flags are retained"
    );
    assert!(
        updated.has_keyword(&Keyword::new("$Fixture").expect("kw")),
        "keywords survive a system-flag update"
    );

    // ---- verify retrieval by UID (the IMAP access pattern) -------------
    let by_uid = h
        .messages
        .get_entry_by_uid(inbox.id, Uid::MIN)
        .await
        .expect("query")
        .expect("entry exists");
    assert_eq!(by_uid.id, entry.id);
    assert_eq!(by_uid.message_id, inserted.message_id);
    assert!(by_uid.flags.has(SystemFlags::FLAGGED));
}

#[tokio::test]
async fn inline_image_message_records_its_content_id() {
    // Inline resources are what a `cid:` reference in HTML mail resolves
    // against; the UI's HTML preview depends on this being retained.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let inserted = h
        .deliver(&common::fixture("valid/inline-image.eml"), &[inbox])
        .await;

    let attachments = h
        .messages
        .attachments(inserted.message_id)
        .await
        .expect("attachments");

    let logo = attachments
        .iter()
        .find(|a| a.content_id.as_deref() == Some("logo-1"))
        .expect("the inline image, addressable by Content-ID");
    assert!(logo.is_inline, "an inline resource is not an attachment");
    assert_eq!(logo.content_type, "image/png");
}

#[tokio::test]
async fn nested_multipart_produces_hierarchical_part_paths() {
    // The paths IMAP FETCH BODY[1.1] will address in Phase 5.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let inserted = h
        .deliver(&common::fixture("valid/nested-multipart.eml"), &[inbox])
        .await;
    let message = h.get_message(inserted.message_id).await;

    let paths: Vec<String> = message
        .parsed
        .parts
        .iter()
        .map(|p| p.path.to_string())
        .collect();

    assert!(paths.contains(&String::from("1")), "got {paths:?}");
    assert!(
        paths.contains(&String::from("1.1")),
        "nested parts must get dotted paths, got {paths:?}"
    );

    // The nested part still slices the original bytes correctly.
    let nested = message
        .parsed
        .part(&sandesh_core::PartPath::parse("1.1").expect("path"))
        .expect("nested part");
    let body = nested.body(message.raw.bytes()).expect("in bounds");
    assert_eq!(body, b"Nested plain text.");
}
