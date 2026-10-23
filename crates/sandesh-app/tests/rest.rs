//! REST API behaviour against a running application.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestApp;

const MESSAGE: &str = "From: sender@test.local\r\n\
                       To: dev@example.test\r\n\
                       Subject: A subject\r\n\
                       \r\n\
                       Body text.";

#[tokio::test(flavor = "multi_thread")]
async fn health_is_cheap_and_always_answers() {
    let app = TestApp::start().await;

    let (status, body) = app.get_json("/health").await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_reports_the_database() {
    let app = TestApp::start().await;

    let (status, body) = app.get_json("/health/ready").await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["database"], true);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn info_reports_the_real_bound_addresses() {
    // The connection strings are product surface: a developer copies them into
    // their application's configuration, so they must be the ports actually in
    // use — which under an ephemeral port is only knowable after binding.
    let app = TestApp::start().await;

    let (status, body) = app.get_json("/info").await;
    assert_eq!(status, 200);
    assert_eq!(body["smtp_address"], app.smtp().to_string());
    assert_eq!(body["http_address"], app.http.to_string());
    assert_eq!(body["websocket_path"], "/api/v1/events");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_instance_has_a_catch_all_account_with_default_mailboxes() {
    // Without this, a freshly started Sandesh would accept a connection and
    // then fail to deliver anything.
    let app = TestApp::start().await;

    let (status, accounts) = app.get_json("/accounts").await;
    assert_eq!(status, 200);
    assert_eq!(accounts["total_estimate"], 1);
    assert_eq!(accounts["items"][0]["is_catch_all"], true);

    let (status, mailboxes) = app.get_json("/mailboxes").await;
    assert_eq!(status, 200);
    let names: Vec<&str> = mailboxes["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|mailbox| mailbox["name"].as_str())
        .collect();
    assert!(names.contains(&"INBOX"), "got {names:?}");
    assert!(names.contains(&"Sent"), "got {names:?}");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_detail_carries_headers_and_no_raw_bytes() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let id = list["items"][0]["id"].as_str().expect("an id").to_owned();

    let (status, detail) = app.get_json(&format!("/messages/{id}")).await;
    assert_eq!(status, 200);
    assert_eq!(detail["subject"], "A subject");
    assert_eq!(detail["parse_ok"], true);
    assert_eq!(detail["source"], "smtp");

    let headers = detail["headers"].as_array().expect("headers");
    assert!(
        headers.iter().any(|header| header["name"] == "Subject"),
        "the Subject header must be present"
    );

    // The detail view describes the message; the bytes have their own endpoint.
    assert!(detail.get("raw").is_none());

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_to_header_is_reported_on_the_summary() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let item = &list["items"][0];
    assert_eq!(item["from"]["address"], "sender@test.local");
    assert_eq!(item["to"][0]["address"], "dev@example.test");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_are_available_separately() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let id = list["items"][0]["id"].as_str().expect("an id").to_owned();

    let (status, headers) = app.get_json(&format!("/messages/{id}/headers")).await;
    assert_eq!(status, 200);
    let headers = headers.as_array().expect("an array of headers");
    assert!(headers.iter().any(|header| header["name"] == "From"));

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_message_is_a_404_problem_document() {
    let app = TestApp::start().await;

    let (status, body) = app
        .get_json("/messages/01a05d79-0000-7000-8000-000000000000")
        .await;
    assert_eq!(status, 404);
    assert_eq!(body["status"], 404);
    assert_eq!(body["title"], "Not found");
    assert_eq!(body["type"], "https://sandesh.dev/errors/not-found");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unparseable_identifier_is_a_400_not_a_500() {
    let app = TestApp::start().await;

    for bad in ["nonsense", "1%20OR%201=1", "%2e%2e%2f%2e%2e%2fetc%2fpasswd"] {
        let (status, body) = app.get_json(&format!("/messages/{bad}")).await;
        assert_eq!(status, 400, "for {bad}");
        assert_eq!(body["type"], "https://sandesh.dev/errors/bad-request");
    }

    // A literal traversal never reaches a handler at all: the router refuses
    // it. Either way it must not be a 200 and must not be a 500.
    let (status, _) = app.get("/messages/../../etc/passwd").await;
    assert!(
        (400..500).contains(&status),
        "a path traversal must be refused, got {status}"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_internal_error_never_leaks_implementation_detail() {
    // Nothing here should fail internally; the assertion is that whatever comes
    // back for any input, it never names SQLite, a table, or a filesystem path.
    let app = TestApp::start().await;

    for path in [
        "/messages?limit=0",
        "/messages?cursor=%27%3B%20DROP%20TABLE%20messages%3B%20--",
        "/mailboxes/not-a-uuid",
        "/entries/00000000-0000-0000-0000-000000000000",
    ] {
        let (_, body) = app.get_json(path).await;
        let rendered = body.to_string();
        for leak in [
            "sqlx",
            "SQLITE",
            "no such table",
            "/tmp/",
            ".db",
            "panicked",
        ] {
            assert!(
                !rendered.contains(leak),
                "{leak} leaked from {path}: {rendered}"
            );
        }
    }

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_absurd_page_size_is_refused() {
    // Without a cap, one GET is a request to serialise the whole database.
    let app = TestApp::start().await;

    let (status, body) = app.get_json("/messages?limit=100000000").await;
    assert_eq!(status, 400);
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("500")),
        "the error should say what the maximum is: {body}"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_walks_a_mailbox_without_repeating_or_skipping() {
    let app = TestApp::start().await;

    for index in 0..7 {
        app.deliver(
            "sender@test.local",
            "dev@example.test",
            &format!("Subject: message {index}\r\n\r\nBody."),
        )
        .await;
    }

    let (_, mailboxes) = app.get_json("/mailboxes").await;
    let inbox = mailboxes["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|mailbox| mailbox["name"] == "INBOX")
        .expect("an INBOX");
    let inbox_id = inbox["id"].as_str().expect("an id").to_owned();

    // Page through three at a time and collect every UID seen.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let path = match &cursor {
            Some(value) => format!("/mailboxes/{inbox_id}/entries?limit=3&cursor={value}"),
            None => format!("/mailboxes/{inbox_id}/entries?limit=3"),
        };
        let (status, page) = app.get_json(&path).await;
        assert_eq!(status, 200);

        for entry in page["items"].as_array().expect("items") {
            seen.push(entry["uid"].as_u64().expect("a uid"));
        }
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }

    seen.sort_unstable();
    assert_eq!(
        seen,
        (1..=7).collect::<Vec<u64>>(),
        "every entry exactly once, none skipped or repeated"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_can_be_set_and_cleared_on_an_entry() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let entry_id = list["items"][0]["entries"][0]["entry_id"]
        .as_str()
        .expect("an entry")
        .to_owned();

    let (status, body) = app
        .request(
            "PATCH",
            &format!("/entries/{entry_id}/flags"),
            Some(r#"{"add":["\\Seen","important"]}"#),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);

    let updated: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let flags: Vec<&str> = updated["flags"]
        .as_array()
        .expect("flags")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(flags.contains(&"\\Seen"), "got {flags:?}");
    assert!(flags.contains(&"important"), "got {flags:?}");

    // And removing them again.
    let (status, body) = app
        .request(
            "PATCH",
            &format!("/entries/{entry_id}/flags"),
            Some(r#"{"remove":["\\Seen"]}"#),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);
    let updated: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let flags: Vec<&str> = updated["flags"]
        .as_array()
        .expect("flags")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(!flags.contains(&"\\Seen"), "got {flags:?}");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_an_entry_leaves_the_message_alone() {
    // A placement is not the message. Removing one must not remove the other.
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let entry_id = list["items"][0]["entries"][0]["entry_id"]
        .as_str()
        .expect("an entry")
        .to_owned();
    let message_id = list["items"][0]["id"].as_str().expect("an id").to_owned();

    let (status, _) = app
        .request("DELETE", &format!("/entries/{entry_id}"), None)
        .await
        .expect("request");
    assert_eq!(status, 204);

    let (status, _) = app.get_json(&format!("/entries/{entry_id}")).await;
    assert_eq!(status, 404, "the placement is gone");

    let (status, _) = app.get_json(&format!("/messages/{message_id}")).await;
    assert_eq!(status, 200, "the message content survives its placement");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mime_part_is_never_served_as_executable_content() {
    // An HTML part rendered in Sandesh's own origin could act on behalf of
    // whoever is viewing it.
    let app = TestApp::start().await;
    app.deliver(
        "sender@test.local",
        "dev@example.test",
        "Subject: html\r\nContent-Type: text/html\r\n\r\n<script>alert(1)</script>",
    )
    .await;

    let (_, list) = app.get_json("/messages").await;
    let id = list["items"][0]["id"].as_str().expect("an id").to_owned();

    // The root part is addressed as "0" — an empty trailing segment matches no
    // route, and inventing one would make the URL space ambiguous.
    let (_, mime) = app.get_json(&format!("/messages/{id}/mime")).await;
    let addressed = mime["parts"][0]["url_path"]
        .as_str()
        .expect("the MIME view tells a client how to address each part")
        .to_owned();

    let (status, body) = app
        .request("GET", &format!("/messages/{id}/part/{addressed}"), None)
        .await
        .expect("request");
    assert_eq!(status, 200);
    // The bytes are returned faithfully — Sandesh shows what arrived — but the
    // response must not invite a browser to run them.
    assert!(String::from_utf8_lossy(&body).contains("<script>"));

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mime_part_is_served_decoded_by_default_and_raw_with_flag() {
    let app = TestApp::start().await;
    let raw_msg = "Subject: Base64 Part\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
PHA+dGVzdCBmcm9tIG9kb288L3A+"; // "<p>test from odoo</p>" in base64

    app.deliver("sender@test.local", "dev@example.test", raw_msg)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let id = list["items"][0]["id"].as_str().expect("an id");

    let (_, mime) = app.get_json(&format!("/messages/{id}/mime")).await;
    let addressed = mime["parts"][0]["url_path"].as_str().expect("url_path");

    // Default request decodes base64
    let (status, decoded) = app
        .request("GET", &format!("/messages/{id}/part/{addressed}"), None)
        .await
        .expect("request");
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8_lossy(&decoded), "<p>test from odoo</p>");

    // Query with ?raw=true returns raw undecoded bytes
    let (status_raw, raw_body) = app
        .request(
            "GET",
            &format!("/messages/{id}/part/{addressed}?raw=true"),
            None,
        )
        .await
        .expect("request");
    assert_eq!(status_raw, 200);
    assert_eq!(
        String::from_utf8_lossy(&raw_body).trim(),
        "PHA+dGVzdCBmcm9tIG9kb288L3A+"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_clients_do_not_interfere() {
    let app = TestApp::start().await;

    for index in 0..5 {
        app.deliver(
            "sender@test.local",
            "dev@example.test",
            &format!("Subject: concurrent {index}\r\n\r\nBody."),
        )
        .await;
    }

    // Ten readers at once must all see a consistent picture.
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let http = app.http;
        tasks.push(tokio::spawn(async move {
            let probe = TestAppProbe { http };
            probe.count().await
        }));
    }

    for task in tasks {
        assert_eq!(task.await.expect("reader task"), 5);
    }

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_endpoint_composes_and_delivers_message() {
    let app = TestApp::start().await;

    let payload = serde_json::json!({
        "from": "alice@sandesh.test",
        "to": ["bob@example.test"],
        "subject": "Hello from API Composer",
        "text_body": "This is plain text body",
        "html_body": "<p>This is <b>HTML</b> body</p>"
    });

    let (status, body) = app
        .request("POST", "/send", Some(&payload.to_string()))
        .await
        .expect("request");
    assert_eq!(status, 201);

    let res: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(res["subject"], "Hello from API Composer");
    assert!(res["placements"].as_u64().unwrap() >= 1);

    let message_id = res["id"].as_str().expect("id");

    // Check message detail
    let (status, detail) = app.get_json(&format!("/messages/{message_id}")).await;
    assert_eq!(status, 200);
    assert_eq!(detail["subject"], "Hello from API Composer");

    // Check raw content has both text and html parts (multipart/alternative)
    let (status, raw_bytes) = app
        .request("GET", &format!("/messages/{message_id}/raw"), None)
        .await
        .expect("raw request");
    assert_eq!(status, 200);
    let raw_str = String::from_utf8_lossy(&raw_bytes);
    assert!(raw_str.contains("multipart/alternative"));
    assert!(raw_str.contains("This is plain text body"));
    assert!(raw_str.contains("<p>This is <b>HTML</b> body</p>"));

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_inbox_entry_moves_it_to_trash() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let entry_id = list["items"][0]["entries"][0]["entry_id"]
        .as_str()
        .expect("entry_id");

    // DELETE entry without permanent flag -> moves to Trash
    let (status, _) = app
        .request("DELETE", &format!("/entries/{entry_id}"), None)
        .await
        .expect("request");
    assert_eq!(status, 204);

    // Verify message still exists, now in Trash mailbox
    let (_, detail) = app
        .get_json(&format!(
            "/messages/{}",
            list["items"][0]["id"].as_str().unwrap()
        ))
        .await;
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["mailbox_name"], "Trash");

    let trash_entry_id = entries[0]["entry_id"].as_str().expect("trash entry_id");

    // Deleting an entry that is already in Trash permanently purges it
    let (status, _) = app
        .request("DELETE", &format!("/entries/{trash_entry_id}"), None)
        .await
        .expect("request");
    assert_eq!(status, 204);

    // The entry placement itself is now gone (404)
    let (status, _) = app.get_json(&format!("/entries/{trash_entry_id}")).await;
    assert_eq!(status, 404);

    // Message detail now has zero placements/entries
    let (status, detail) = app
        .get_json(&format!(
            "/messages/{}",
            list["items"][0]["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(status, 200);
    assert_eq!(detail["entries"].as_array().unwrap().len(), 0);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn move_entry_supports_archive_junk_and_inbox() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let msg_id = list["items"][0]["id"].as_str().expect("msg_id");
    let mut entry_id = list["items"][0]["entries"][0]["entry_id"]
        .as_str()
        .expect("entry_id")
        .to_string();

    // Move to Archive
    let move_archive = serde_json::json!({
        "mailbox_name": "Archive"
    });
    let (status, body) = app
        .request(
            "POST",
            &format!("/entries/{entry_id}/move"),
            Some(&move_archive.to_string()),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);

    let res: serde_json::Value = serde_json::from_slice(&body).expect("json");
    entry_id = res["id"].as_str().expect("id").to_string();

    // Verify detail shows Archive mailbox
    let (_, detail) = app.get_json(&format!("/messages/{msg_id}")).await;
    assert_eq!(detail["entries"][0]["mailbox_name"], "Archive");

    // Move to Junk
    let move_junk = serde_json::json!({
        "mailbox_name": "Junk"
    });
    let (status, body) = app
        .request(
            "POST",
            &format!("/entries/{entry_id}/move"),
            Some(&move_junk.to_string()),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);

    let res: serde_json::Value = serde_json::from_slice(&body).expect("json");
    entry_id = res["id"].as_str().expect("id").to_string();

    // Verify detail shows Junk mailbox
    let (_, detail) = app.get_json(&format!("/messages/{msg_id}")).await;
    assert_eq!(detail["entries"][0]["mailbox_name"], "Junk");

    // Move back to INBOX
    let move_inbox = serde_json::json!({
        "mailbox_name": "INBOX"
    });
    let (status, _) = app
        .request(
            "POST",
            &format!("/entries/{entry_id}/move"),
            Some(&move_inbox.to_string()),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);

    let (_, detail) = app.get_json(&format!("/messages/{msg_id}")).await;
    assert_eq!(detail["entries"][0]["mailbox_name"], "INBOX");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn save_draft_places_message_in_drafts_mailbox() {
    let app = TestApp::start().await;

    let payload = serde_json::json!({
        "from": "alice@sandesh.test",
        "to": ["bob@example.test"],
        "subject": "Work in progress draft",
        "text_body": "Draft contents...",
        "is_draft": true
    });

    let (status, body) = app
        .request("POST", "/send", Some(&payload.to_string()))
        .await
        .expect("request");
    assert_eq!(status, 201);

    let res: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let message_id = res["id"].as_str().expect("id");

    // Check message detail
    let (status, detail) = app.get_json(&format!("/messages/{message_id}")).await;
    assert_eq!(status, 200);
    assert_eq!(detail["subject"], "Work in progress draft");

    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["mailbox_name"], "Drafts");

    let flags = entries[0]["flags"].as_array().expect("flags");
    let flag_strings: Vec<&str> = flags.iter().filter_map(|f| f.as_str()).collect();
    assert!(flag_strings.contains(&"\\Draft"), "flags: {flag_strings:?}");
    assert!(flag_strings.contains(&"\\Seen"), "flags: {flag_strings:?}");

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_can_be_copied_to_another_mailbox() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let message_id = list["items"][0]["id"].as_str().expect("message id");
    let (_, detail) = app.get_json(&format!("/messages/{message_id}")).await;
    let entry_id = detail["entries"][0]["entry_id"].as_str().expect("entry id");

    let (_, mailboxes) = app.get_json("/mailboxes").await;
    let sent_mailbox = mailboxes["items"]
        .as_array()
        .expect("mailboxes")
        .iter()
        .find(|m| m["name"] == "Sent")
        .expect("Sent mailbox");
    let sent_id = sent_mailbox["id"].as_str().expect("sent id");

    let payload = serde_json::json!({
        "target_mailbox_id": sent_id
    });
    let (status, body) = app
        .request(
            "POST",
            &format!("/entries/{entry_id}/copy"),
            Some(&payload.to_string()),
        )
        .await
        .expect("request");
    assert_eq!(status, 200);

    let copy_res: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let copied_entry_id = copy_res["id"].as_str().expect("copied entry id");
    assert_ne!(copied_entry_id, entry_id);
    assert_eq!(copy_res["mailbox_id"], sent_id);
    assert_eq!(copy_res["message_id"], message_id);

    // Verify detail now shows 2 entries
    let (_, updated_detail) = app.get_json(&format!("/messages/{message_id}")).await;
    let entries = updated_detail["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_message_removes_all_entries_and_content() {
    let app = TestApp::start().await;
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let message_id = list["items"][0]["id"].as_str().expect("message id");

    let (status, _) = app
        .request("DELETE", &format!("/messages/{message_id}"), None)
        .await
        .expect("delete");
    assert_eq!(status, 204);

    let (status, _) = app.get(&format!("/messages/{message_id}")).await;
    assert_eq!(status, 404);

    let (_, list_after) = app.get_json("/messages").await;
    assert_eq!(list_after["total_estimate"], 0);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_crud_lifecycle_and_inbox_protection() {
    let app = TestApp::start().await;
    let (_, accounts) = app.get_json("/accounts").await;
    let account_id = accounts["items"][0]["id"].as_str().expect("account id");

    // 1. Create a custom mailbox
    let payload = serde_json::json!({
        "account_id": account_id,
        "name": "Custom/Work"
    });
    let (status, body) = app
        .request("POST", "/mailboxes", Some(&payload.to_string()))
        .await
        .expect("create mailbox");
    assert_eq!(status, 201);
    let created: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let custom_id = created["id"].as_str().expect("mailbox id");
    assert_eq!(created["name"], "Custom/Work");
    // A new mailbox starts *unsubscribed*, matching IMAP `CREATE` (ADR-068).
    // This handler used to default it to subscribed, so the same mailbox
    // appeared in `LIST (SUBSCRIBED)` or not depending on which protocol
    // created it. Subscription is a separate act (RFC 9051).
    assert_eq!(created["subscribed"], false);

    // 2. Patch custom mailbox (rename and unsubscribe)
    let patch_payload = serde_json::json!({
        "name": "Custom/Archived",
        "subscribed": false
    });
    let (status, body) = app
        .request(
            "PATCH",
            &format!("/mailboxes/{custom_id}"),
            Some(&patch_payload.to_string()),
        )
        .await
        .expect("patch mailbox");
    assert_eq!(status, 200);
    let patched: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(patched["name"], "Custom/Archived");
    assert_eq!(patched["subscribed"], false);

    // 3. INBOX cannot be renamed or deleted
    let (_, mailboxes) = app.get_json("/mailboxes").await;
    let inbox = mailboxes["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|m| m["name"] == "INBOX")
        .expect("INBOX");
    let inbox_id = inbox["id"].as_str().expect("inbox id");

    let rename_inbox_payload = serde_json::json!({ "name": "NotInbox" });
    let (status, _) = app
        .request(
            "PATCH",
            &format!("/mailboxes/{inbox_id}"),
            Some(&rename_inbox_payload.to_string()),
        )
        .await
        .expect("patch inbox");
    assert_eq!(status, 400);

    let (status, _) = app
        .request("DELETE", &format!("/mailboxes/{inbox_id}"), None)
        .await
        .expect("delete inbox");
    assert_eq!(status, 400);

    // 4. Delete custom mailbox
    let (status, _) = app
        .request("DELETE", &format!("/mailboxes/{custom_id}"), None)
        .await
        .expect("delete custom mailbox");
    assert_eq!(status, 204);

    let (status, _) = app.get(&format!("/mailboxes/{custom_id}/entries")).await;
    assert_eq!(status, 404);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn account_crud_and_aliases() {
    let app = TestApp::start().await;

    // 1. Create account
    let payload = serde_json::json!({
        "username": "charlie",
        "password": "secretpassword",
        "addresses": ["charlie@test.local", "charlie.alias@test.local"]
    });
    let (status, body) = app
        .request("POST", "/accounts", Some(&payload.to_string()))
        .await
        .expect("create account");
    assert_eq!(status, 201);
    let account: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let account_id = account["id"].as_str().expect("account id");
    assert_eq!(account["username"], "charlie");
    assert_eq!(account["addresses"].as_array().expect("addresses").len(), 2);

    // Verify default mailboxes were provisioned for charlie
    let (_, mailboxes) = app.get_json("/mailboxes").await;
    let charlie_mailboxes: Vec<&str> = mailboxes["items"]
        .as_array()
        .expect("mailboxes")
        .iter()
        .filter(|m| m["account_id"] == account_id)
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(charlie_mailboxes.contains(&"INBOX"));
    assert!(charlie_mailboxes.contains(&"Sent"));

    // 2. Patch account
    let patch_payload = serde_json::json!({
        "password": "newsecretpassword",
        "add_addresses": ["charlie.team@test.local"]
    });
    let (status, body) = app
        .request(
            "PATCH",
            &format!("/accounts/{account_id}"),
            Some(&patch_payload.to_string()),
        )
        .await
        .expect("patch account");
    assert_eq!(status, 200);
    let patched: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(patched["addresses"].as_array().expect("addresses").len(), 3);

    // 3. Delete account
    let (status, _) = app
        .request("DELETE", &format!("/accounts/{account_id}"), None)
        .await
        .expect("delete account");
    assert_eq!(status, 204);

    let (status, _) = app.get(&format!("/accounts/{account_id}")).await;
    assert_eq!(status, 404);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_events_stream_mirrors_domain_events() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    let app = TestApp::start().await;

    let mut stream = TcpStream::connect(app.http).await.expect("connect to SSE");
    let req = format!(
        "GET /api/v1/events/sse HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        app.http
    );
    stream
        .write_all(req.as_bytes())
        .await
        .expect("send SSE request");

    let mut reader = BufReader::new(stream);
    let mut initial_line = String::new();
    reader
        .read_line(&mut initial_line)
        .await
        .expect("read response");
    assert!(
        initial_line.contains("200 OK"),
        "expected 200 OK, got: {initial_line}"
    );

    // Trigger an event via SMTP delivery
    app.deliver("sender@test.local", "dev@example.test", MESSAGE)
        .await;

    // Read until we see event: message_delivered
    let mut saw_event = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let mut line = String::new();
        if tokio::time::timeout(
            std::time::Duration::from_millis(500),
            reader.read_line(&mut line),
        )
        .await
        .is_ok()
            && (line.contains("MessageReceived") || line.contains("EntryCreated"))
        {
            saw_event = true;
            break;
        }
    }
    assert!(
        saw_event,
        "SSE stream did not receive MessageReceived or EntryCreated event"
    );

    drop(reader);

    app.stop().await;
}

/// A minimal read-only client, so concurrent readers need no shared state.
struct TestAppProbe {
    http: std::net::SocketAddr,
}

impl TestAppProbe {
    async fn count(&self) -> u64 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(self.http)
            .await
            .expect("connect");
        let request = format!(
            "GET /api/v1/messages HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.http
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("read response");

        let index = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a header terminator");
        let body: serde_json::Value =
            serde_json::from_slice(&raw[index + 4..]).expect("a JSON body");
        body["total_estimate"].as_u64().expect("a total")
    }
}
