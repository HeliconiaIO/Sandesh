//! The HTTP surface as a client actually meets it (Phase 4).
//!
//! These start a real [`Application`] on ephemeral ports and drive it over TCP,
//! so what is asserted is the behaviour a browser gets: the routing precedence
//! between the API and the SPA fallback, the security headers, and the
//! generated contract being served by the same binary it was generated from.
//!
//! Ephemeral ports throughout (§66): a fixed port makes a test suite fail when
//! a developer happens to be running Sandesh.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// A running application, torn down when dropped.
struct Harness {
    http: SocketAddr,
    smtp: SocketAddr,
    shutdown: CancellationToken,
    _dir: TempDir,
}

impl Harness {
    async fn start() -> Self {
        let dir = TempDir::new().expect("a temporary directory");

        // Port 0: the OS picks, so parallel tests never collide.
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().expect("a valid address");
        let defaults = AppConfig::default();
        let config = AppConfig {
            database_path: dir.path().join("sandesh.db"),
            blob_path: dir.path().join("blobs"),
            http: sandesh_config::HttpConfig {
                bind: ephemeral,
                ..defaults.http
            },
            smtp: sandesh_config::SmtpConfig {
                bind: ephemeral,
                ..defaults.smtp
            },
            // IMAP and POP3 need the same treatment: their defaults are *fixed* ports, so
            // inheriting them would make parallel tests in this file race
            // and fail with `AddrInUse`.
            imap: sandesh_config::ImapConfig {
                bind: Some(ephemeral),
                ..defaults.imap
            },
            pop3: sandesh_config::Pop3Config {
                bind: Some(ephemeral),
                ..defaults.pop3
            },
            ..defaults
        };

        let app = Application::start(config).await.expect("the app starts");
        let http = app.addresses().http.expect("HTTP is bound");
        let smtp = app.addresses().smtp.expect("SMTP is bound");
        let shutdown = app.shutdown_token();

        tokio::spawn(async move {
            let _ = app.run().await;
        });

        Self {
            http,
            smtp,
            shutdown,
            _dir: dir,
        }
    }

    /// Issues a bare HTTP/1.1 GET and returns the whole response.
    ///
    /// Hand-rolled rather than pulling in an HTTP client: the assertions are
    /// about status lines and headers, and a client would abstract away exactly
    /// what is being tested.
    async fn get(&self, path: &str) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("writes the request");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Issues an HTTP/1.1 request with an arbitrary method and a JSON body.
    ///
    /// `PATCH` has no dedicated helper because only these tests need it, and a
    /// general one keeps the hand-rolled request in a single place.
    async fn post_method(&self, method: &str, path: &str, body: &[u8]) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let headers = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("writes the request headers");
        stream.write_all(body).await.expect("writes the body");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Issues an HTTP/1.1 POST with body and returns the whole response.
    async fn post(&self, path: &str, body: &[u8], content_type: &str) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let headers = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("writes the request headers");
        stream.write_all(body).await.expect("writes the body");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Issues an HTTP/1.1 DELETE and returns the whole response.
    async fn delete(&self, path: &str) -> String {
        self.post_method("DELETE", path, b"").await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[tokio::test]
async fn the_openapi_document_is_served_by_the_running_server() {
    // The contract a client can fetch must be the contract the server was built
    // from — not a file someone remembered to upload.
    let app = Harness::start().await;
    let response = app.get("/api/openapi.json").await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("application/json"));

    let body = response.split("\r\n\r\n").nth(1).expect("a body");
    let document: serde_json::Value = serde_json::from_str(body).expect("valid JSON");
    assert!(
        document["openapi"]
            .as_str()
            .is_some_and(|version| version.starts_with("3.1"))
    );
    assert!(document["paths"]["/api/v1/messages"]["get"].is_object());
}

/// Reads the JSON body of a response.
fn body_of(response: &str) -> serde_json::Value {
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn documented_response_shapes_match_what_handlers_send() {
    // The contract is only worth having if it describes the real API. utoipa
    // cannot check an annotation against its handler's return type, so a
    // response documented as an object while the handler sends an array
    // compiles cleanly and produces TypeScript that fails at runtime — which
    // is exactly the drift this whole phase exists to prevent.
    //
    // This asserts the property structurally: for each endpoint, whether the
    // live response is an array or an object must match what the document says.
    let app = Harness::start().await;

    let document = body_of(&app.get("/api/openapi.json").await);

    // Endpoints reachable without any stored mail.
    for path in [
        "/api/v1/accounts",
        "/api/v1/mailboxes",
        "/api/v1/messages",
        "/api/v1/health",
        "/api/v1/info",
    ] {
        let live = body_of(&app.get(path).await);

        let schema = &document["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"];

        let documents_array = schema["type"] == "array";
        assert_eq!(
            documents_array,
            live.is_array(),
            "{path}: the document says array={documents_array} but the server sent \
             array={}. The #[utoipa::path] annotation disagrees with the handler.",
            live.is_array()
        );

        // An object response documented as a list envelope must really carry
        // the envelope's fields.
        if let Some(reference) = schema["$ref"].as_str()
            && reference.contains("ListResponse")
        {
            assert!(
                live.get("items").is_some(),
                "{path} is documented as a list envelope but sent no `items`"
            );
        }
    }
}

#[tokio::test]
async fn the_headers_endpoint_sends_the_shape_it_documents() {
    // Regression guard for a real mismatch: `/headers` was documented as a
    // paginated envelope while the handler returned a bare array, so the
    // generated client read `.items` from an array and found nothing.
    let app = Harness::start().await;

    let document = body_of(&app.get("/api/openapi.json").await);
    let schema = &document["paths"]["/api/v1/messages/{id}/headers"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"];

    assert_eq!(
        schema["type"], "array",
        "the headers endpoint returns a bare array and must be documented as one"
    );
}

#[tokio::test]
async fn the_spa_fallback_never_swallows_the_api() {
    // The failure this guards against is subtle and severe: if the fallback
    // caught `/api/...`, an unknown endpoint would return an HTML page with a
    // 200, and every client would see "success" for a route that does not
    // exist (§45).
    let app = Harness::start().await;

    let response = app.get("/api/v1/no-such-endpoint").await;
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "an unknown API path must 404, got: {response}"
    );
    assert!(
        !response.contains("<!doctype html>"),
        "an API path must never receive the SPA shell"
    );
}

#[tokio::test]
async fn a_deep_link_reaches_the_application() {
    // A pasted or refreshed URL must load the app rather than 404 (§45).
    let app = Harness::start().await;
    let response = app.get("/mailbox/some-id/message/other-id").await;

    let status_ok = response.starts_with("HTTP/1.1 200");
    let not_built = response.starts_with("HTTP/1.1 503");

    // 503 is the honest answer when `npm run build` has not been run; either
    // way the route was handled by the asset layer, not by a 404.
    assert!(
        status_ok || not_built,
        "a client-side route must reach the SPA handler, got: {response}"
    );
}

#[tokio::test]
async fn asset_responses_carry_the_security_headers() {
    // The CSP is what keeps email-borne HTML from executing in this origin, so
    // its presence is asserted on the real response rather than only in a unit
    // test of the constant (§73, §104).
    let app = Harness::start().await;
    let response = app.get("/").await.to_lowercase();

    assert!(
        response.contains("content-security-policy"),
        "every document response must carry a CSP"
    );
    assert!(response.contains("frame-ancestors 'none'"), "clickjacking");
    assert!(response.contains("x-content-type-options: nosniff"));
    assert!(
        response.contains("script-src 'self'") && !response.contains("unsafe-eval"),
        "inline and eval'd script must stay forbidden"
    );
}

#[tokio::test]
async fn info_reports_the_security_posture_honestly() {
    // A developer must never be unsure whether their local mail server accepts
    // anonymous mail. The default answer is that it does (ADR-005), and the API
    // must say so plainly rather than omitting the field.
    let app = Harness::start().await;
    let info = body_of(&app.get("/api/v1/info").await);

    assert_eq!(
        info["smtp_auth"], "disabled",
        "authentication must remain disabled by default (ADR-005)"
    );
    assert_eq!(info["smtp_tls"], "disabled");
    // No TLS configured, so there is no certificate to fingerprint.
    assert!(info["tls_fingerprint"].is_null());
}

#[tokio::test]
async fn no_response_ever_carries_a_credential() {
    // The blunt end of "no plaintext credential exposure" (ADR-035): sweep the
    // read surface and assert that nothing resembling a password or a private
    // key appears in any of it.
    let app = Harness::start().await;

    // Secrets, in any form, anywhere on the read surface.
    const SECRETS: [&str; 4] = [
        "credential_plain",
        "begin private key",
        "begin rsa private key",
        "$argon2",
    ];

    for path in [
        "/api/v1/info",
        "/api/v1/health",
        "/api/v1/accounts",
        "/api/v1/mailboxes",
        "/api/v1/messages",
        "/api/openapi.json",
    ] {
        let response = app.get(path).await.to_lowercase();
        for forbidden in SECRETS {
            assert!(!response.contains(forbidden), "{path} leaked `{forbidden}`");
        }
    }

    // The word "password" is a leak on a data endpoint but not in the schema
    // document, where `CreateAccountRequest`/`PatchAccountRequest` must name
    // the request field they accept. Excluding openapi.json wholesale would
    // blind the sweep above, so only this one token is scoped away from it.
    //
    // `has_password` is exempt, and the exemption is narrow by construction:
    // it is a boolean derived from `credential.is_some()`, so the client can
    // show whether authentication is configured without the hash ever being
    // serialised. Stripping the key before the search keeps the assertion
    // below honest — a real `"password":"…"` pair still fails it, and so does
    // any new field whose name merely contains the word.
    for path in [
        "/api/v1/info",
        "/api/v1/health",
        "/api/v1/accounts",
        "/api/v1/mailboxes",
        "/api/v1/messages",
    ] {
        let response = app
            .get(path)
            .await
            .to_lowercase()
            .replace("\"has_password\"", "\"has_credential_flag\"");
        assert!(!response.contains("password"), "{path} leaked `password`");
    }

    // The exemption above must never widen into "any field carrying a value".
    // A boolean is safe; a string is not, whatever the field is called.
    let accounts = app.get("/api/v1/accounts").await.to_lowercase();
    let compact: String = accounts.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !compact.contains(r#""has_password":""#),
        "/api/v1/accounts carries a credential value, not a boolean flag"
    );

    // What the schema may do is *name* the field; what it must never do is
    // carry a value for one. A `"password":"…"` pair is a real leak, where a
    // bare `"password"` key introducing a schema object is not.
    let document = app.get("/api/openapi.json").await.to_lowercase();
    let compact: String = document.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        !compact.contains(r#""password":""#),
        "/api/openapi.json carries a password value, not just the field name"
    );
}

#[tokio::test]
async fn cors_stays_closed_by_default() {
    // ADR-022: a wildcard on a local mail server would let any page a developer
    // visits read their mail. Phase 4 must not have relaxed it for convenience.
    let app = Harness::start().await;
    let response = app.get("/api/v1/health").await.to_lowercase();

    assert!(response.starts_with("http/1.1 200"), "{response}");
    assert!(
        !response.contains("access-control-allow-origin: *"),
        "a CORS wildcard must never be sent"
    );
}

#[tokio::test]
async fn a_message_sent_over_smtp_becomes_visible_over_http() {
    // The authoritative flow (§67): SMTP → core → storage → REST. Nothing is
    // seeded into SQLite directly, because a test that writes to the database
    // proves the database works, not that Sandesh does.
    let app = Harness::start().await;

    let mut smtp = TcpStream::connect(app.smtp)
        .await
        .expect("connects to SMTP");
    let mut buffer = [0_u8; 1024];

    async fn expect(stream: &mut TcpStream, buffer: &mut [u8], prefix: &str) {
        let read = stream.read(buffer).await.expect("reads a reply");
        let reply = String::from_utf8_lossy(&buffer[..read]).into_owned();
        assert!(reply.starts_with(prefix), "expected {prefix}, got {reply}");
    }

    expect(&mut smtp, &mut buffer, "220").await;

    for (command, code) in [
        ("EHLO test.local\r\n", "250"),
        ("MAIL FROM:<sender@example.test>\r\n", "250"),
        ("RCPT TO:<dev@example.test>\r\n", "250"),
        ("DATA\r\n", "354"),
    ] {
        smtp.write_all(command.as_bytes()).await.expect("writes");
        expect(&mut smtp, &mut buffer, code).await;
    }

    smtp.write_all(
        b"From: sender@example.test\r\n\
          To: dev@example.test\r\n\
          Subject: Phase 4 acceptance\r\n\
          \r\n\
          The body of the message.\r\n\
          .\r\n",
    )
    .await
    .expect("writes the message");
    expect(&mut smtp, &mut buffer, "250").await;

    smtp.write_all(b"QUIT\r\n").await.expect("writes QUIT");

    // The REST surface must now show it. A short poll because delivery and the
    // HTTP read are separate operations, not because either is slow.
    let mut found = false;
    for _ in 0..40 {
        let response = app.get("/api/v1/messages").await;
        if response.contains("Phase 4 acceptance") {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(found, "a message accepted over SMTP must appear over REST");
}

#[tokio::test]
async fn direct_message_injection_and_search_and_explain() {
    let app = Harness::start().await;

    let raw_msg = b"From: alice@example.com\r\n\
                    To: bob@example.com\r\n\
                    Subject: Direct Injected Fixture\r\n\
                    \r\n\
                    Hello from direct raw injection!\r\n";

    let response = app
        .post("/api/v1/messages", raw_msg, "message/rfc822")
        .await;
    assert!(response.starts_with("HTTP/1.1 201"), "got: {response}");
    assert!(response.contains("Direct Injected Fixture"));

    // Explain search
    let explain_res = app
        .get("/api/v1/search/explain?q=from:alice%20has:attachment")
        .await;
    assert!(
        explain_res.starts_with("HTTP/1.1 200"),
        "got: {explain_res}"
    );
    let explain_json = body_of(&explain_res);
    assert!(
        explain_json["plan"]
            .as_str()
            .unwrap_or("")
            .contains("HasAttachment")
    );

    // Search matches
    let search_res = app.get("/api/v1/search?q=subject:Fixture").await;
    assert!(search_res.starts_with("HTTP/1.1 200"), "got: {search_res}");
    let search_json = body_of(&search_res);
    assert_eq!(search_json["items"].as_array().map(|a| a.len()), Some(1));

    // Search does not match
    let no_match_res = app.get("/api/v1/search?q=subject:NonExistent").await;
    assert!(
        no_match_res.starts_with("HTTP/1.1 200"),
        "got: {no_match_res}"
    );
    let no_match_json = body_of(&no_match_res);
    assert_eq!(no_match_json["items"].as_array().map(|a| a.len()), Some(0));
}

#[tokio::test]
async fn bulk_entries_mark_read_and_delete() {
    let app = Harness::start().await;

    let raw_msg1 = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: Bulk 1\r\n\r\nTest 1\r\n";
    let raw_msg2 = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: Bulk 2\r\n\r\nTest 2\r\n";

    let r1 = app
        .post("/api/v1/messages", raw_msg1, "message/rfc822")
        .await;
    let r2 = app
        .post("/api/v1/messages", raw_msg2, "message/rfc822")
        .await;
    assert!(r1.starts_with("HTTP/1.1 201"));
    assert!(r2.starts_with("HTTP/1.1 201"));

    let list_res = app.get("/api/v1/messages").await;
    let list_json = body_of(&list_res);
    let items = list_json["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);

    let mut entry_ids = Vec::new();
    for item in items {
        if let Some(entries) = item["entries"].as_array() {
            for entry in entries {
                if let Some(id) = entry["entry_id"].as_str() {
                    entry_ids.push(id.to_string());
                }
            }
        }
    }
    assert_eq!(entry_ids.len(), 2);

    // Bulk mark as read
    let bulk_read_req = serde_json::json!({
        "entry_ids": entry_ids,
        "action": "mark_read"
    });
    let bulk_res = app
        .post(
            "/api/v1/entries/bulk",
            bulk_read_req.to_string().as_bytes(),
            "application/json",
        )
        .await;
    assert!(bulk_res.starts_with("HTTP/1.1 200"), "got: {bulk_res}");
    let bulk_json = body_of(&bulk_res);
    assert_eq!(bulk_json["affected"].as_u64(), Some(2));

    // Verify search is:read finds them
    let search_res = app.get("/api/v1/search?q=is:read").await;
    let search_json = body_of(&search_res);
    assert_eq!(search_json["items"].as_array().map(|a| a.len()), Some(2));

    // Bulk delete
    let bulk_del_req = serde_json::json!({
        "entry_ids": entry_ids,
        "action": "delete"
    });
    let del_res = app
        .post(
            "/api/v1/entries/bulk",
            bulk_del_req.to_string().as_bytes(),
            "application/json",
        )
        .await;
    assert!(del_res.starts_with("HTTP/1.1 200"), "got: {del_res}");
    let del_json = body_of(&del_res);
    assert_eq!(del_json["affected"].as_u64(), Some(2));
}

// ---------------------------------------------------------------------------
// Account isolation across the entry-placement endpoints
// ---------------------------------------------------------------------------
//
// `/entries/{id}/copy` and `/move` accept a destination as *either* a mailbox
// name or a mailbox id. The name form is resolved inside the source entry's
// account, so it cannot escape one; the id form is supplied by the caller and
// names a mailbox directly, which is the form that has to be checked rather
// than trusted.
//
// IMAP cannot express this mistake: a destination there is a name resolved
// against the authenticated session's account, so the account is never a
// parameter. Over HTTP it is, and mail belonging to one account must not be
// placed into another's mailbox — the isolation the whole account model exists
// to provide, and what `sandesh-smtp/tests/isolation.rs` protects on the
// delivery side.

/// Two accounts, each with its own mailbox, and one message in the first.
///
/// Returns `(entry id in account A, a mailbox id belonging to account B)`.
async fn two_accounts_one_message(app: &Harness) -> (String, String) {
    let alice = app
        .post(
            "/api/v1/accounts",
            serde_json::json!({ "username": "alice@example.com",
                                "addresses": ["alice@example.com"] })
            .to_string()
            .as_bytes(),
            "application/json",
        )
        .await;
    assert!(alice.starts_with("HTTP/1.1 201"), "got: {alice}");

    let bob = app
        .post(
            "/api/v1/accounts",
            serde_json::json!({ "username": "bob@example.com",
                                "addresses": ["bob@example.com"] })
            .to_string()
            .as_bytes(),
            "application/json",
        )
        .await;
    assert!(bob.starts_with("HTTP/1.1 201"), "got: {bob}");
    let bob_id = body_of(&bob)["id"]
        .as_str()
        .expect("the new account reports its id")
        .to_owned();

    // A mailbox owned by Bob. Accounts are provisioned with the default set, so
    // Archive already exists and is his.
    let bob_mailboxes = app
        .get(&format!("/api/v1/mailboxes?account_id={bob_id}"))
        .await;
    let bob_target = body_of(&bob_mailboxes)["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|m| m["name"].as_str() == Some("Archive"))
                .or_else(|| items.first())
        })
        .and_then(|m| m["id"].as_str())
        .expect("Bob has at least one mailbox")
        .to_owned();

    // Mail addressed to Alice, so the placement lands in her account.
    let raw = b"From: outside@example.net\r\n\
                To: alice@example.com\r\n\
                Subject: Isolation fixture\r\n\
                \r\n\
                Body.\r\n";
    let injected = app.post("/api/v1/messages", raw, "message/rfc822").await;
    assert!(injected.starts_with("HTTP/1.1 201"), "got: {injected}");

    let entry_id = body_of(&injected)["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .and_then(|entry| entry["entry_id"].as_str())
        .expect("the injected message reports its placement")
        .to_owned();

    (entry_id, bob_target)
}

/// How many placements a mailbox holds, by id.
async fn entry_count(app: &Harness, mailbox_id: &str) -> usize {
    let response = app
        .get(&format!("/api/v1/mailboxes/{mailbox_id}/entries"))
        .await;
    body_of(&response)["items"].as_array().map_or(0, Vec::len)
}

#[tokio::test]
async fn copying_an_entry_into_another_accounts_mailbox_is_refused() {
    let app = Harness::start().await;
    let (entry_id, foreign_mailbox) = two_accounts_one_message(&app).await;

    let before = entry_count(&app, &foreign_mailbox).await;

    let response = app
        .post(
            &format!("/api/v1/entries/{entry_id}/copy"),
            serde_json::json!({ "target_mailbox_id": foreign_mailbox })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;

    // A client error, not a 500: the request is understood and refused, and it
    // must not surface as an internal failure from a storage constraint.
    assert!(
        response.starts_with("HTTP/1.1 4"),
        "a cross-account copy must be refused with a client error, got: {response}"
    );

    // The refusal has to be real, not merely reported. This is the assertion
    // that would catch a check that returns an error *after* writing.
    assert_eq!(
        entry_count(&app, &foreign_mailbox).await,
        before,
        "no placement may appear in the other account's mailbox"
    );
}

#[tokio::test]
async fn moving_an_entry_into_another_accounts_mailbox_is_refused() {
    let app = Harness::start().await;
    let (entry_id, foreign_mailbox) = two_accounts_one_message(&app).await;

    let before = entry_count(&app, &foreign_mailbox).await;

    let response = app
        .post(
            &format!("/api/v1/entries/{entry_id}/move"),
            serde_json::json!({ "target_mailbox_id": foreign_mailbox })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;

    assert!(
        response.starts_with("HTTP/1.1 4"),
        "a cross-account move must be refused with a client error, got: {response}"
    );
    assert_eq!(
        entry_count(&app, &foreign_mailbox).await,
        before,
        "no placement may appear in the other account's mailbox"
    );

    // A move is destructive at the source, so a refused move must also leave
    // the original where it was rather than losing the mail in between.
    let still_there = app.get(&format!("/api/v1/entries/{entry_id}")).await;
    assert!(
        still_there.starts_with("HTTP/1.1 200"),
        "a refused move must not remove the source placement, got: {still_there}"
    );
}

#[tokio::test]
async fn a_bulk_move_cannot_reach_across_accounts() {
    // The bulk path resolves destinations separately from the single-entry
    // handlers, so it needs its own assertion — it reports per-entry success
    // rather than failing the request, and "affected" must not count a
    // placement it was not allowed to make.
    let app = Harness::start().await;
    let (entry_id, foreign_mailbox) = two_accounts_one_message(&app).await;

    let before = entry_count(&app, &foreign_mailbox).await;

    let response = app
        .post(
            "/api/v1/entries/bulk",
            serde_json::json!({
                "entry_ids": [entry_id],
                "action": { "move": { "target_mailbox_id": foreign_mailbox } },
            })
            .to_string()
            .as_bytes(),
            "application/json",
        )
        .await;
    assert!(response.starts_with("HTTP/1.1 200"), "got: {response}");

    assert_eq!(
        body_of(&response)["affected"].as_u64(),
        Some(0),
        "a cross-account move must not be counted as applied"
    );
    assert_eq!(
        entry_count(&app, &foreign_mailbox).await,
        before,
        "no placement may appear in the other account's mailbox"
    );
}

#[tokio::test]
async fn a_destination_named_by_id_within_the_same_account_still_works() {
    // The guard must refuse the other account, not every id — without this the
    // fix could pass the tests above by rejecting the id form outright.
    let app = Harness::start().await;
    let (entry_id, _) = two_accounts_one_message(&app).await;

    let entry = body_of(&app.get(&format!("/api/v1/entries/{entry_id}")).await);
    let source_mailbox = entry["mailbox_id"]
        .as_str()
        .expect("the entry reports its mailbox");
    let account = body_of(
        &app.get(&format!("/api/v1/mailboxes/{source_mailbox}"))
            .await,
    )["account_id"]
        .as_str()
        .expect("the mailbox reports its account")
        .to_owned();

    let own_target = body_of(
        &app.get(&format!("/api/v1/mailboxes?account_id={account}"))
            .await,
    )["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|m| m["id"].as_str() != Some(source_mailbox))
        })
        .and_then(|m| m["id"].as_str())
        .expect("the account has a second mailbox")
        .to_owned();

    let response = app
        .post(
            &format!("/api/v1/entries/{entry_id}/copy"),
            serde_json::json!({ "target_mailbox_id": own_target })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "a copy inside one account must still succeed, got: {response}"
    );
    assert_eq!(entry_count(&app, &own_target).await, 1);
}

#[tokio::test]
async fn copying_an_entry_into_its_own_mailbox_is_refused() {
    // IMAP refuses this rather than letting the placement uniqueness index
    // decide (ADR-050/ADR-051). Over HTTP it currently reaches storage, which
    // turns a client mistake into a 500.
    let app = Harness::start().await;
    let (entry_id, _) = two_accounts_one_message(&app).await;

    let entry = body_of(&app.get(&format!("/api/v1/entries/{entry_id}")).await);
    let own_mailbox = entry["mailbox_id"]
        .as_str()
        .expect("the entry reports its mailbox")
        .to_owned();

    let response = app
        .post(
            &format!("/api/v1/entries/{entry_id}/copy"),
            serde_json::json!({ "target_mailbox_id": own_mailbox })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 4"),
        "copying into the source mailbox is a client error, not a server fault, got: {response}"
    );
}

#[tokio::test]
async fn a_destination_that_does_not_exist_is_reported_as_not_found() {
    // An unknown id must not reach storage and fail on a foreign key: the
    // caller gets told the destination is missing.
    let app = Harness::start().await;
    let (entry_id, _) = two_accounts_one_message(&app).await;

    let response = app
        .post(
            &format!("/api/v1/entries/{entry_id}/copy"),
            serde_json::json!({ "target_mailbox_id": "0192f3a1-0000-7000-8000-000000000000" })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "an unknown destination is a 404, got: {response}"
    );
}

// ---------------------------------------------------------------------------
// INBOX is protected identically on both surfaces
// ---------------------------------------------------------------------------
//
// IMAP has always refused to create, delete or rename INBOX (RFC 9051 §6.3.4-6).
// The HTTP handlers enforced only part of that: creating INBOX was not refused
// at all, and renaming a mailbox *onto* INBOX was not either, because only the
// mailbox's current name was checked. Both now go through `MailboxService`, so
// the rule has one implementation (ADR-068).
//
// The stakes are the one-INBOX-per-account invariant: `MailboxService::inbox_of`
// treats a missing INBOX as unrecoverable, and delivery resolves every
// recipient through it.

#[tokio::test]
async fn creating_inbox_over_http_is_refused() {
    let app = Harness::start().await;
    let accounts = body_of(&app.get("/api/v1/accounts").await);
    let account = accounts["items"][0]["id"]
        .as_str()
        .expect("the bootstrap account exists");

    for spelling in ["INBOX", "inbox", "Inbox"] {
        let response = app
            .post(
                "/api/v1/mailboxes",
                serde_json::json!({ "account_id": account, "name": spelling })
                    .to_string()
                    .as_bytes(),
                "application/json",
            )
            .await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "creating {spelling} must be refused, got: {response}"
        );
    }

    // Exactly one INBOX, whatever was attempted.
    let mailboxes = body_of(
        &app.get(&format!("/api/v1/mailboxes?account_id={account}"))
            .await,
    );
    let inboxes = mailboxes["items"]
        .as_array()
        .expect("a mailbox list")
        .iter()
        .filter(|m| {
            m["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case("INBOX"))
        })
        .count();
    assert_eq!(inboxes, 1, "the account must still have exactly one INBOX");
}

#[tokio::test]
async fn renaming_a_mailbox_onto_inbox_is_refused() {
    // The gap that existed only over HTTP: the source name was checked, the
    // target was not, so any mailbox could be renamed to INBOX.
    let app = Harness::start().await;
    let accounts = body_of(&app.get("/api/v1/accounts").await);
    let account = accounts["items"][0]["id"].as_str().expect("an account");

    let created = app
        .post(
            "/api/v1/mailboxes",
            serde_json::json!({ "account_id": account, "name": "Scratch" })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(created.starts_with("HTTP/1.1 201"), "got: {created}");
    let scratch = body_of(&created)["id"]
        .as_str()
        .expect("the new mailbox reports its id")
        .to_owned();

    let response = app
        .post_method(
            "PATCH",
            &format!("/api/v1/mailboxes/{scratch}"),
            serde_json::json!({ "name": "INBOX" })
                .to_string()
                .as_bytes(),
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "renaming onto INBOX must be refused, got: {response}"
    );

    let mailboxes = body_of(
        &app.get(&format!("/api/v1/mailboxes?account_id={account}"))
            .await,
    );
    let inboxes = mailboxes["items"]
        .as_array()
        .expect("a mailbox list")
        .iter()
        .filter(|m| {
            m["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case("INBOX"))
        })
        .count();
    assert_eq!(inboxes, 1, "there must still be exactly one INBOX");
}

#[tokio::test]
async fn a_mailbox_created_over_http_starts_unsubscribed() {
    // IMAP `CREATE` leaves a mailbox unsubscribed and this handler used to
    // default it to subscribed, so the same mailbox appeared in
    // `LIST (SUBSCRIBED)` or not depending on which protocol created it.
    // IMAP's behaviour is canonical (ADR-068).
    let app = Harness::start().await;
    let accounts = body_of(&app.get("/api/v1/accounts").await);
    let account = accounts["items"][0]["id"].as_str().expect("an account");

    let created = app
        .post(
            "/api/v1/mailboxes",
            serde_json::json!({ "account_id": account, "name": "Unsubscribed/Probe" })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(created.starts_with("HTTP/1.1 201"), "got: {created}");
    assert_eq!(body_of(&created)["subscribed"], serde_json::json!(false));
}

#[tokio::test]
async fn creating_a_mailbox_twice_is_a_conflict_not_a_server_error() {
    // The duplicate name used to reach the storage unique index, so a client
    // mistake surfaced as a 500. The service checks first.
    let app = Harness::start().await;
    let accounts = body_of(&app.get("/api/v1/accounts").await);
    let account = accounts["items"][0]["id"].as_str().expect("an account");

    let body = serde_json::json!({ "account_id": account, "name": "Twice" }).to_string();
    let first = app
        .post("/api/v1/mailboxes", body.as_bytes(), "application/json")
        .await;
    assert!(first.starts_with("HTTP/1.1 201"), "got: {first}");

    let second = app
        .post("/api/v1/mailboxes", body.as_bytes(), "application/json")
        .await;
    assert!(
        second.starts_with("HTTP/1.1 409"),
        "a duplicate name is a conflict, got: {second}"
    );
}

// ---------------------------------------------------------------------------
// An account must be usable once created
// ---------------------------------------------------------------------------
//
// Accounts are created from three places — SMTP's `auto_create`, this endpoint,
// and first-run bootstrap — and each used to build one itself. The HTTP version
// accepted two shapes that cannot work: an account with no address, which no
// mail can ever be routed to, and a second catch-all, when routing assumes at
// most one. All three now go through `AccountService` (ADR-070).

#[tokio::test]
async fn an_account_with_no_address_is_refused() {
    // Mail is routed by identity, so this account would be inert: created
    // successfully and unable to receive anything.
    let app = Harness::start().await;

    let before = body_of(&app.get("/api/v1/accounts").await)["items"]
        .as_array()
        .map_or(0, Vec::len);

    for payload in [
        serde_json::json!({ "username": "nobody" }),
        serde_json::json!({ "username": "nobody", "addresses": [] }),
        // Blank entries are dropped, which must still leave no address.
        serde_json::json!({ "username": "nobody", "addresses": ["", "  "] }),
    ] {
        let response = app
            .post(
                "/api/v1/accounts",
                payload.to_string().as_bytes(),
                "application/json",
            )
            .await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "an unreachable account must be refused, got: {response}"
        );
    }

    let after = body_of(&app.get("/api/v1/accounts").await)["items"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(after, before, "no account may have been created");
}

#[tokio::test]
async fn a_second_catch_all_account_is_refused() {
    // Bootstrap creates one at first run, and `AccountStore::catch_all` answers
    // with a single account — so a second makes routing depend on which the
    // store happens to return.
    let app = Harness::start().await;

    let response = app
        .post(
            "/api/v1/accounts",
            serde_json::json!({
                "username": "second-catch-all",
                "addresses": ["anything@example.test"],
                "is_catch_all": true,
            })
            .to_string()
            .as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 409"),
        "a second catch-all is a conflict, got: {response}"
    );

    let accounts = body_of(&app.get("/api/v1/accounts").await);
    let catch_alls = accounts["items"]
        .as_array()
        .expect("an account list")
        .iter()
        .filter(|a| a["is_catch_all"] == serde_json::json!(true))
        .count();
    assert_eq!(catch_alls, 1, "there must still be exactly one catch-all");
}

#[tokio::test]
async fn a_blank_username_is_refused() {
    let app = Harness::start().await;

    let response = app
        .post(
            "/api/v1/accounts",
            serde_json::json!({ "username": "   ", "addresses": ["a@example.test"] })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "a blank username must be refused, got: {response}"
    );
}

#[tokio::test]
async fn a_duplicate_username_is_a_conflict() {
    // Previously this reached the storage unique index, so a client mistake
    // surfaced as a 500.
    let app = Harness::start().await;
    let body = serde_json::json!({
        "username": "alice",
        "addresses": ["alice@example.test"],
    })
    .to_string();

    let first = app
        .post("/api/v1/accounts", body.as_bytes(), "application/json")
        .await;
    assert!(first.starts_with("HTTP/1.1 201"), "got: {first}");

    let second = app
        .post("/api/v1/accounts", body.as_bytes(), "application/json")
        .await;
    assert!(
        second.starts_with("HTTP/1.1 409"),
        "a duplicate username is a conflict, got: {second}"
    );
}

#[tokio::test]
async fn workspaces_http_lifecycle_and_safety() {
    let app = Harness::start().await;

    // 1. List initial workspaces — default workspace must exist and be active.
    let list_resp = app.get("/api/v1/workspaces").await;
    assert!(list_resp.starts_with("HTTP/1.1 200"), "got: {list_resp}");
    assert!(
        list_resp.contains("\"id\":\"default\""),
        "missing default workspace: {list_resp}"
    );
    assert!(
        list_resp.contains("\"is_active\":true"),
        "default workspace should be active: {list_resp}"
    );

    // 2. Creating a workspace with an empty name is refused with 400.
    let bad_create = app
        .post(
            "/api/v1/workspaces",
            serde_json::json!({ "name": "   " }).to_string().as_bytes(),
            "application/json",
        )
        .await;
    assert!(bad_create.starts_with("HTTP/1.1 400"), "got: {bad_create}");

    // 3. Create a new isolated workspace.
    let create_body = serde_json::json!({
        "name": "Billing Service",
        "id": "billing-service",
        "description": "Invoice and receipt testing",
        "set_active": false,
    })
    .to_string();

    let create_resp = app
        .post(
            "/api/v1/workspaces",
            create_body.as_bytes(),
            "application/json",
        )
        .await;
    assert!(
        create_resp.starts_with("HTTP/1.1 201"),
        "got: {create_resp}"
    );
    assert!(create_resp.contains("\"id\":\"billing-service\""));
    assert!(create_resp.contains("\"is_active\":false"));

    // 4. Fetch the single workspace by ID.
    let get_resp = app.get("/api/v1/workspaces/billing-service").await;
    assert!(get_resp.starts_with("HTTP/1.1 200"), "got: {get_resp}");
    assert!(get_resp.contains("\"name\":\"Billing Service\""));

    // 5. Activate the workspace.
    let activate_resp = app
        .post(
            "/api/v1/workspaces/billing-service/activate",
            b"{}",
            "application/json",
        )
        .await;
    assert!(
        activate_resp.starts_with("HTTP/1.1 200"),
        "got: {activate_resp}"
    );
    assert!(activate_resp.contains("\"is_active\":true"));

    // 6. Purge the workspace.
    let purge_resp = app
        .post(
            "/api/v1/workspaces/billing-service/purge",
            serde_json::json!({ "mode": "mail_and_traces" })
                .to_string()
                .as_bytes(),
            "application/json",
        )
        .await;
    assert!(purge_resp.starts_with("HTTP/1.1 200"), "got: {purge_resp}");
    assert!(purge_resp.contains("\"workspace_id\":\"billing-service\""));

    // 7. Export the database file.
    let export_resp = app.get("/api/v1/workspaces/billing-service/export").await;
    assert!(
        export_resp.starts_with("HTTP/1.1 200"),
        "got: {export_resp}"
    );
    assert!(
        export_resp.contains("content-type: application/x-sqlite3")
            || export_resp.contains("Content-Type: application/x-sqlite3"),
        "expected application/x-sqlite3 header: {export_resp}"
    );

    // 8. Cannot delete the default workspace — must return 400.
    let delete_default = app.delete("/api/v1/workspaces/default").await;
    assert!(
        delete_default.starts_with("HTTP/1.1 400"),
        "deleting default workspace must return 400, got: {delete_default}"
    );

    // 9. Delete the custom workspace — returns 204.
    let delete_custom = app.delete("/api/v1/workspaces/billing-service").await;
    assert!(
        delete_custom.starts_with("HTTP/1.1 204"),
        "deleting custom workspace must return 204, got: {delete_custom}"
    );

    // 10. Fetching deleted workspace returns 404.
    let get_deleted = app.get("/api/v1/workspaces/billing-service").await;
    assert!(
        get_deleted.starts_with("HTTP/1.1 404"),
        "fetching deleted workspace must return 404, got: {get_deleted}"
    );
}
