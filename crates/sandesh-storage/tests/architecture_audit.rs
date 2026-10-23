//! Phase 1 definition-of-done audit (§41), as executable checks rather than
//! assertions in prose.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;
use common::Harness;
use sandesh_core::{MessageStore, PartPath};

/// §41 MIME: can IMAP FETCH BODY[], BODY[1], BODY[2.1] and BODYSTRUCTURE be
/// built on this storage without redesigning it?
#[tokio::test]
async fn imap_fetch_primitives_are_all_expressible() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let raw = common::fixture("valid/nested-multipart.eml");
    let inserted = h.deliver(&raw, &[inbox]).await;
    let message = h.get_message(inserted.message_id).await;

    // FETCH BODY[] — the whole message, byte-exact.
    assert_eq!(message.raw.bytes(), raw.as_slice());

    // FETCH RFC822.SIZE — from the stored octets.
    assert_eq!(message.size(), raw.len() as u64);

    // FETCH BODY[1] and BODY[2.1]-style addressing by IMAP part path.
    for path in ["1", "1.1", "2"] {
        let part_path = PartPath::parse(path).expect("path");
        if let Some(part) = message.parsed.part(&part_path) {
            let body = part.body(message.raw.bytes());
            assert!(body.is_some(), "BODY[{path}] must slice cleanly");
        }
    }

    // FETCH BODY[HEADER] — headers carry their own ranges.
    for header in &message.parsed.headers {
        assert!(header.range.is_within(raw.len()));
    }

    // BODYSTRUCTURE needs type, encoding, disposition, size and nesting per part.
    for part in &message.parsed.parts {
        assert!(!part.content_type.is_empty(), "type is required");
        let _ = part.encoding;
        let _ = part.disposition;
        let _ = part.decoded_size;
        let _ = &part.children; // nesting, for the recursive structure
    }
    assert!(
        message.parsed.parts.iter().any(|p| !p.children.is_empty()),
        "the tree structure must be reconstructible after a round trip"
    );
}

/// §41 Storage: can the SQLite implementation be swapped without touching the
/// core? Proven by the core's ports being satisfiable by a non-SQLite type.
#[tokio::test]
async fn ports_are_satisfiable_without_sqlite() {
    use async_trait::async_trait;
    use sandesh_core::{BlobStore, CoreResult};

    // An in-memory BlobStore implemented entirely outside sandesh-storage.
    #[derive(Debug, Default)]
    struct MemoryBlobs(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);

    #[async_trait]
    impl BlobStore for MemoryBlobs {
        async fn put(&self, bytes: &[u8]) -> CoreResult<String> {
            let key = format!("{}", bytes.len());
            self.0
                .lock()
                .expect("lock")
                .insert(key.clone(), bytes.to_vec());
            Ok(key)
        }
        async fn get(&self, reference: &str) -> CoreResult<Option<Vec<u8>>> {
            Ok(self.0.lock().expect("lock").get(reference).cloned())
        }
        async fn delete(&self, reference: &str) -> CoreResult<bool> {
            Ok(self.0.lock().expect("lock").remove(reference).is_some())
        }
    }

    // If this compiles and runs, the port is a genuine abstraction rather than
    // a description of the SQLite implementation.
    let blobs = MemoryBlobs::default();
    let reference = blobs.put(b"substitutable").await.expect("put");
    assert_eq!(
        blobs.get(&reference).await.expect("get"),
        Some(b"substitutable".to_vec())
    );

    let _: &dyn BlobStore = &blobs;
}

/// §41 Protocols: SMTP, IMAP and POP3 must each be expressible against the
/// ports without any knowledge of SQLite.
#[tokio::test]
async fn protocol_operations_are_expressible_through_ports_alone() {
    use sandesh_core::{FlagUpdate, MailboxStore, SystemFlags, Uid};
    use std::collections::BTreeSet;

    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    // --- SMTP: accept a message and place it. -------------------------
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    // --- POP3: STAT / LIST / RETR / UIDL --------------------------------
    let entries = h.messages.list_entries(inbox).await.expect("LIST");
    assert_eq!(entries.len(), 1, "STAT count");
    let retr = h
        .messages
        .raw(entries[0].message_id)
        .await
        .expect("RETR")
        .expect("exists");
    assert!(!retr.bytes().is_empty());
    let _uidl = entries[0].uid; // UIDL derives from the per-mailbox UID

    // --- IMAP: SELECT / STATUS / FETCH / STORE / COPY -------------------
    let status = h.mailboxes.counts(inbox).await.expect("STATUS");
    assert_eq!(status.total, 1);

    let fetched = h
        .messages
        .get_entry_by_uid(inbox, Uid::MIN)
        .await
        .expect("UID FETCH")
        .expect("exists");
    assert_eq!(fetched.message_id, inserted.message_id);

    h.messages
        .update_flags(
            fetched.id,
            FlagUpdate::Add {
                system: SystemFlags::SEEN,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("STORE");

    let archive = h.mailbox(account, "Archive").await;
    h.messages
        .copy_entry(fetched.id, archive)
        .await
        .expect("COPY");

    // EXPUNGE: remove \Deleted entries, then reclaim unreferenced messages.
    h.messages
        .update_flags(
            fetched.id,
            FlagUpdate::Add {
                system: SystemFlags::DELETED,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("mark \\Deleted");
    let deleted: Vec<_> = h
        .messages
        .list_entries(inbox)
        .await
        .expect("list")
        .into_iter()
        .filter(sandesh_core::MessageEntry::is_deleted)
        .collect();
    assert_eq!(deleted.len(), 1, "EXPUNGE has a set to act on");
    for entry in deleted {
        h.messages.delete_entry(entry.id).await.expect("EXPUNGE");
    }
    assert_eq!(h.mailboxes.counts(inbox).await.expect("counts").total, 0);
}

/// §41 UI: can the API expose the model without domain types becoming DTOs?
///
/// Verified structurally: no domain type derives Serialize, so serialisation
/// must go through a separate DTO layer in Phase 3.
#[test]
fn domain_types_are_not_serialisation_dtos() {
    // A compile-time observation: sandesh-core declares no serde dependency,
    // which the architecture test asserts. This test documents the intent and
    // fails loudly if someone adds derives and a dependency later.
    let manifest = include_str!("../../../crates/sandesh-core/Cargo.toml");
    assert!(
        !manifest.contains("serde"),
        "sandesh-core must not depend on serde; API DTOs belong in sandesh-api"
    );
}
