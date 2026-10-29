//! The FTS5 search index (`STORAGE_DESIGN.md` §7, `ROADMAP.md` gap 4).
//!
//! The index is a **prefilter**: it may narrow the set of messages a search
//! must load, and it may never change what that search returns. So the tests
//! that matter are not "does FTS5 find things" but:
//!
//!  1. **Parity** — for every query, filtering by the index gives exactly the
//!     same messages as scanning everything. This is the whole contract, and it
//!     is checked over the real fixture corpus, not over hand-picked strings.
//!  2. **No false negatives on mid-token substrings** — the specific error the
//!     originally specified `unicode61` tokenizer would have made silently.
//!  3. **Degradation** — `NullSearchIndex` and an out-of-date index must both
//!     still produce correct results, only slower.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    Message, MessageId, NullSearchIndex, SearchCandidates, SearchIndex, SearchQuery, TextField,
};
use sandesh_storage::SqliteSearchIndex;
use std::collections::BTreeSet;

/// A body whose interesting substring starts mid-token.
///
/// "foo" appears in "xfoobar" but no *word* begins with it. A `unicode61`
/// index would exclude this message from a `BODY "foo"` search — the false
/// negative this index exists to avoid — while `SearchQuery::matches` includes
/// it, because RFC 9051 §6.4.4 says substring.
const MID_TOKEN: &[u8] = b"\
From: sender@example.test\r\n\
To: dev@example.test\r\n\
Subject: Nothing notable\r\n\
\r\n\
The body mentions xfoobar exactly once.\r\n";

const PLAIN: &[u8] = b"\
From: alice@example.test\r\n\
To: dev@example.test\r\n\
Subject: Quarterly report\r\n\
\r\n\
Numbers are attached.\r\n";

/// Builds a harness with an index, delivers the given messages into INBOX, and
/// indexes each. Returns the harness, the index, and the stored messages.
async fn indexed(bodies: &[&[u8]]) -> (Harness, SqliteSearchIndex, Vec<Message>) {
    let harness = Harness::new().await;
    let account = harness.account("dev", "dev@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let index = SqliteSearchIndex::new(harness.db.pool().clone());

    let mut stored = Vec::new();
    for body in bodies {
        let inserted = harness.deliver(body, &[inbox]).await;
        let message = harness.get_message(inserted.message_id).await;
        index.index(&message).await.expect("index message");
        stored.push(message);
    }
    (harness, index, stored)
}

/// The ids a query matches when every message is loaded and checked — the
/// authoritative answer the index must not change.
fn scanned(messages: &[Message], query: &SearchQuery) -> BTreeSet<MessageId> {
    messages
        .iter()
        .filter(|message| {
            // A placement is required by `matches`; flags and UID play no part
            // in the text queries under test, so any entry serves.
            let entry = entry_for(message);
            query.matches(Some(message), &entry)
        })
        .map(|message| message.id)
        .collect()
}

/// The ids a query matches when the index is consulted first, then `matches`
/// confirms each candidate — the path production takes.
fn with_index(
    messages: &[Message],
    query: &SearchQuery,
    candidates: &SearchCandidates,
) -> BTreeSet<MessageId> {
    messages
        .iter()
        .filter(|message| candidates.admits(message.id))
        .filter(|message| query.matches(Some(message), &entry_for(message)))
        .map(|message| message.id)
        .collect()
}

fn entry_for(message: &Message) -> sandesh_core::MessageEntry {
    sandesh_core::MessageEntry {
        id: sandesh_core::EntryId::new(),
        message_id: message.id,
        mailbox_id: sandesh_core::MailboxId::new(),
        uid: sandesh_core::Uid::new(1).expect("uid 1 is valid"),
        flags: sandesh_core::MessageFlags::empty(),
        internal_date: message.received_at,
        added_at: message.received_at,
        mod_seq: sandesh_core::ModSeq::MIN,
    }
}

#[tokio::test]
async fn a_mid_token_substring_is_never_missed() {
    let (_h, index, messages) = indexed(&[MID_TOKEN, PLAIN]).await;

    let query = SearchQuery::Contains {
        field: TextField::Body,
        value: "foo".to_owned(),
    };

    // The message must match on the authoritative path.
    let truth = scanned(&messages, &query);
    assert_eq!(
        truth.len(),
        1,
        "substring semantics must find 'foo' in 'xfoobar'"
    );

    // And the index must not exclude it. This is the assertion that fails
    // outright with a word tokenizer.
    let candidates = index.candidates(&query).await.expect("candidates");
    assert!(
        candidates.admits(*truth.iter().next().expect("one match")),
        "the index excluded a message that matches: false negative"
    );
    assert_eq!(with_index(&messages, &query, &candidates), truth);
}

#[tokio::test]
async fn the_index_narrows_without_changing_results_over_the_corpus() {
    // A spread of real fixtures rather than strings chosen to pass. Named
    // explicitly and loaded without a fallback: a missing fixture must fail
    // this test, not silently shrink it to nothing.
    let names = [
        "valid/attachment.eml",
        "valid/cc-and-bcc-headers.eml",
        "valid/custom-headers.eml",
        "encodings/base64-body.eml",
        "encodings/encoded-header-base64.eml",
        "malformed/body-only.eml",
        "real_world/gmail-reply.eml",
        "real_world/django-password-reset.eml",
    ];
    let bodies: Vec<Vec<u8>> = names.iter().map(|name| common::fixture(name)).collect();
    let refs: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
    assert_eq!(refs.len(), names.len(), "every named fixture must load");

    let (_h, index, messages) = indexed(&refs).await;

    // Terms drawn from the corpus plus terms that match nothing, across every
    // field, including the ones the index refuses to narrow.
    let terms = ["the", "report", "example.test", "zzzznotpresent", "a", "@"];
    let fields = [
        TextField::Subject,
        TextField::From,
        TextField::To,
        TextField::Cc,
        TextField::Body,
        TextField::Text,
        TextField::Bcc,
        TextField::Header("x-mailer".to_owned()),
    ];

    for field in &fields {
        for term in &terms {
            let query = SearchQuery::Contains {
                field: field.clone(),
                value: (*term).to_owned(),
            };
            let candidates = index.candidates(&query).await.expect("candidates");
            assert_eq!(
                with_index(&messages, &query, &candidates),
                scanned(&messages, &query),
                "index changed the result for {field:?} {term:?}"
            );
        }
    }
}

#[tokio::test]
async fn boolean_combinations_keep_parity() {
    let (_h, index, messages) = indexed(&[MID_TOKEN, PLAIN]).await;

    let body = |v: &str| SearchQuery::Contains {
        field: TextField::Body,
        value: v.to_owned(),
    };
    let subject = |v: &str| SearchQuery::Contains {
        field: TextField::Subject,
        value: v.to_owned(),
    };

    let queries = vec![
        SearchQuery::And(vec![body("foo"), subject("Nothing")]),
        SearchQuery::Or(vec![body("foo"), subject("Quarterly")]),
        // `Not` must never be narrowed: complementing a superset yields a
        // subset, which is the forbidden direction.
        SearchQuery::Not(Box::new(body("foo"))),
        // An `Or` with an unnarrowable branch must abandon narrowing entirely.
        SearchQuery::Or(vec![
            body("foo"),
            SearchQuery::Contains {
                field: TextField::Header("x-spam".to_owned()),
                value: "yes".to_owned(),
            },
        ]),
        SearchQuery::And(vec![body("foo"), SearchQuery::HasAttachment]),
    ];

    for query in queries {
        let candidates = index.candidates(&query).await.expect("candidates");
        assert_eq!(
            with_index(&messages, &query, &candidates),
            scanned(&messages, &query),
            "index changed the result for {query:?}"
        );
    }
}

#[tokio::test]
async fn a_negation_is_never_narrowed() {
    let (_h, index, _messages) = indexed(&[MID_TOKEN, PLAIN]).await;

    let query = SearchQuery::Not(Box::new(SearchQuery::Contains {
        field: TextField::Body,
        value: "foo".to_owned(),
    }));

    assert_eq!(
        index.candidates(&query).await.expect("candidates"),
        SearchCandidates::Unsupported,
        "narrowing a negation inverts the safety property"
    );
}

#[tokio::test]
async fn a_stale_index_still_returns_correct_results() {
    let (harness, index, mut messages) = indexed(&[PLAIN]).await;
    let account = harness.account("other", "other@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;

    // Delivery indexes automatically, so staleness has to be forced. Removing
    // the index row while the message stays put is exactly the state a crash
    // between the two writes would leave if they were not one transaction, and
    // the state a partial rebuild passes through.
    let inserted = harness.deliver(MID_TOKEN, &[inbox]).await;
    messages.push(harness.get_message(inserted.message_id).await);
    index
        .remove(inserted.message_id)
        .await
        .expect("force the index out of date");

    let query = SearchQuery::Contains {
        field: TextField::Body,
        value: "foo".to_owned(),
    };

    // The index must decline rather than answer from partial data.
    let candidates = index.candidates(&query).await.expect("candidates");
    assert_eq!(
        candidates,
        SearchCandidates::Unsupported,
        "an index missing a message must not narrow"
    );
    assert_eq!(
        with_index(&messages, &query, &candidates),
        scanned(&messages, &query)
    );
}

#[tokio::test]
async fn the_null_index_leaves_search_unchanged() {
    let (_h, _index, messages) = indexed(&[MID_TOKEN, PLAIN]).await;
    let null = NullSearchIndex;

    let query = SearchQuery::Contains {
        field: TextField::Body,
        value: "foo".to_owned(),
    };
    let candidates = null.candidates(&query).await.expect("candidates");

    assert_eq!(candidates, SearchCandidates::Unsupported);
    assert_eq!(
        with_index(&messages, &query, &candidates),
        scanned(&messages, &query),
        "removing the index must change performance, not results"
    );
}

#[tokio::test]
async fn reindexing_replaces_rather_than_duplicating() {
    let (_h, index, messages) = indexed(&[PLAIN]).await;
    let message = &messages[0];

    for _ in 0..3 {
        index.index(message).await.expect("re-index");
    }

    let query = SearchQuery::Contains {
        field: TextField::Subject,
        value: "Quarterly".to_owned(),
    };
    let candidates = index.candidates(&query).await.expect("candidates");
    let SearchCandidates::Candidates(ids) = candidates else {
        panic!("expected the index to narrow this query");
    };
    assert_eq!(ids.len(), 1, "re-indexing duplicated the message");
}

#[tokio::test]
async fn removing_a_message_removes_it_from_the_index() {
    let (_h, index, messages) = indexed(&[PLAIN]).await;
    let id = messages[0].id;

    index.remove(id).await.expect("remove");

    // Removing again is not an error: the index is derived data.
    index.remove(id).await.expect("remove is idempotent");

    // With the message gone from the index but still in `messages`, the index
    // must decline to narrow rather than report it absent.
    let query = SearchQuery::Contains {
        field: TextField::Subject,
        value: "Quarterly".to_owned(),
    };
    assert_eq!(
        index.candidates(&query).await.expect("candidates"),
        SearchCandidates::Unsupported
    );
}

#[tokio::test]
async fn short_values_fall_back_to_a_scan() {
    let (_h, index, _messages) = indexed(&[PLAIN]).await;

    for value in ["", "a", "ab"] {
        let query = SearchQuery::Contains {
            field: TextField::Subject,
            value: value.to_owned(),
        };
        assert_eq!(
            index.candidates(&query).await.expect("candidates"),
            SearchCandidates::Unsupported,
            "a trigram index cannot answer {value:?}"
        );
    }
}

/// The index must actually narrow, or it is dead weight that passes every
/// parity test by declining to do anything.
///
/// This is the counterweight to the parity tests: those prove the index cannot
/// be *wrong*, and this proves it is not merely *absent*.
#[tokio::test]
async fn the_index_actually_narrows_the_candidate_set() {
    let (_h, index, messages) = indexed(&[MID_TOKEN, PLAIN]).await;
    assert_eq!(messages.len(), 2);

    // A term in exactly one of the two messages.
    let query = SearchQuery::Contains {
        field: TextField::Subject,
        value: "Quarterly".to_owned(),
    };

    let SearchCandidates::Candidates(ids) = index.candidates(&query).await.expect("candidates")
    else {
        panic!("the index declined to narrow a query it is built to answer");
    };
    assert_eq!(
        ids.len(),
        1,
        "the index returned {} candidates out of 2; it is not narrowing",
        ids.len()
    );

    // And a term in neither, which must narrow to nothing at all — the case
    // that saves the most work.
    let absent = SearchQuery::Contains {
        field: TextField::Body,
        value: "zzzznotpresent".to_owned(),
    };
    let SearchCandidates::Candidates(none) = index.candidates(&absent).await.expect("candidates")
    else {
        panic!("the index declined to narrow an absent term");
    };
    assert!(
        none.is_empty(),
        "a term in no message must narrow to nothing"
    );
}

/// The index must store the **decoded** body, not the wire bytes.
///
/// `SearchQuery::matches` compares against decoded text, so an index holding
/// base64 would miss every encoded message — a false negative that only shows
/// up on encoded mail, which is most real mail.
#[tokio::test]
async fn an_encoded_body_is_indexed_as_decoded_text() {
    // "Distinctivewordhere" base64-encoded.
    const ENCODED: &[u8] = b"\
From: sender@example.test\r\n\
To: dev@example.test\r\n\
Subject: Encoded\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
RGlzdGluY3RpdmV3b3JkaGVyZQ==\r\n";

    let (_h, index, messages) = indexed(&[ENCODED]).await;

    let query = SearchQuery::Contains {
        field: TextField::Body,
        value: "distinctivewordhere".to_owned(),
    };

    // The authoritative path decodes, so it matches.
    let truth = scanned(&messages, &query);
    assert_eq!(truth.len(), 1, "matches must see the decoded body");

    // The index must agree rather than excluding it.
    let candidates = index.candidates(&query).await.expect("candidates");
    assert_eq!(
        with_index(&messages, &query, &candidates),
        truth,
        "the index holds encoded bytes, not decoded text"
    );
}

/// Delivering a message must index it, with no separate call.
///
/// This is what makes the index useful in production rather than only in tests
/// that remember to index: if `insert` did not maintain the index, every
/// delivered message would be unindexed and `narrow_text` would correctly
/// refuse to narrow anything, leaving a fully built index that never fires.
#[tokio::test]
async fn delivering_a_message_indexes_it() {
    let harness = Harness::new().await;
    let account = harness.account("dev", "dev@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let index = SqliteSearchIndex::new(harness.db.pool().clone());

    // Delivered through the ordinary path. Nothing calls `index`.
    harness.deliver(PLAIN, &[inbox]).await;

    let query = SearchQuery::Contains {
        field: TextField::Subject,
        value: "Quarterly".to_owned(),
    };
    let SearchCandidates::Candidates(ids) = index.candidates(&query).await.expect("candidates")
    else {
        panic!("delivery left the message unindexed, so the index cannot narrow");
    };
    assert_eq!(ids.len(), 1, "the delivered message was not indexed");
}

/// Collecting an orphaned message must remove it from the index too.
#[tokio::test]
async fn collecting_an_orphan_removes_its_index_row() {
    use sandesh_core::MessageStore;

    let harness = Harness::new().await;
    let account = harness.account("dev", "dev@example.test").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let index = SqliteSearchIndex::new(harness.db.pool().clone());

    let inserted = harness.deliver(PLAIN, &[inbox]).await;
    for entry in &inserted.entries {
        harness
            .messages
            .delete_entry(entry.id)
            .await
            .expect("delete entry");
    }
    harness
        .messages
        .collect_orphans()
        .await
        .expect("collect orphans");

    // No messages remain, so nothing is unindexed and the index may narrow —
    // to the empty set, because its row went with the message.
    let query = SearchQuery::Contains {
        field: TextField::Subject,
        value: "Quarterly".to_owned(),
    };
    let SearchCandidates::Candidates(ids) = index.candidates(&query).await.expect("candidates")
    else {
        panic!("expected narrowing once every message is gone");
    };
    assert!(
        ids.is_empty(),
        "the index kept a row for a collected message"
    );
}

/// A text body with a PDF and a CSV attached. The PDF's bytes contain an ASCII
/// word, as real PDFs do; the CSV is text a developer would search.
const WITH_ATTACHMENTS: &[u8] = b"\
From: billing@example.test\r\n\
To: dev@example.test\r\n\
Subject: Invoice\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
Your invoice is attached.\r\n\
--b\r\n\
Content-Type: application/pdf; name=invoice.pdf\r\n\
Content-Disposition: attachment; filename=invoice.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQKcGRmb25seXdvcmQKJSVFT0YK\r\n\
--b\r\n\
Content-Type: text/csv; name=lines.csv\r\n\
Content-Disposition: attachment; filename=lines.csv\r\n\
\r\n\
sku,csvonlyword\r\n\
--b--\r\n";

/// The PDF's text (`%PDF-1.4 pdfonlyword %%EOF`) is invisible to body search
/// and absent from the index; the CSV is searchable through both.
#[tokio::test]
async fn binary_attachments_are_neither_searched_nor_indexed() {
    let (harness, index, messages) = indexed(&[WITH_ATTACHMENTS]).await;

    let body: String = sqlx::query_scalar("SELECT body_text FROM messages_fts")
        .fetch_one(harness.db.pool())
        .await
        .expect("one index row");
    assert!(
        !body.contains("pdfonlyword"),
        "binary part was indexed: {body:?}"
    );
    assert!(
        body.contains("csvonlyword"),
        "text attachment was not indexed"
    );

    for (word, expected) in [("pdfonlyword", 0), ("csvonlyword", 1)] {
        let query = SearchQuery::Contains {
            field: TextField::Body,
            value: word.to_owned(),
        };
        let truth = scanned(&messages, &query);
        assert_eq!(truth.len(), expected, "{word}: matcher");
        let candidates = index.candidates(&query).await.expect("candidates");
        assert_eq!(
            with_index(&messages, &query, &candidates),
            truth,
            "{word}: index"
        );
    }
}

/// Migration 0010 empties the index, `unindexed` then reports every message
/// for the repair task, and search stays correct until the rebuild is done.
#[tokio::test]
async fn migration_0010_empties_the_index_for_the_repair_task() {
    let (harness, index, messages) = indexed(&[WITH_ATTACHMENTS, PLAIN]).await;
    assert!(index.unindexed(10).await.expect("unindexed").is_empty());

    sqlx::raw_sql(include_str!(
        "../../../migrations/0010_text_only_search_index.sql"
    ))
    .execute(harness.db.pool())
    .await
    .expect("re-run migration 0010");

    let missing: BTreeSet<MessageId> = index
        .unindexed(10)
        .await
        .expect("unindexed")
        .into_iter()
        .collect();
    let all: BTreeSet<MessageId> = messages.iter().map(|message| message.id).collect();
    assert_eq!(missing, all, "the rebuild starts from an empty index");

    // While it is missing, a text search declines to narrow rather than
    // excluding it.
    let query = SearchQuery::Contains {
        field: TextField::Body,
        value: "invoice is attached".to_owned(),
    };
    assert!(matches!(
        index.candidates(&query).await.expect("candidates"),
        SearchCandidates::Unsupported
    ));

    for message in &messages {
        index.index(message).await.expect("repair");
    }
    assert!(index.unindexed(10).await.expect("unindexed").is_empty());
    let candidates = index.candidates(&query).await.expect("candidates");
    assert_eq!(
        with_index(&messages, &query, &candidates),
        scanned(&messages, &query)
    );
}
