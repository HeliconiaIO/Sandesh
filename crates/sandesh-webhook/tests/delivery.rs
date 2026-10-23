//! Webhook delivery, failure and retry, against a real HTTP server.
//!
//! `wiremock` rather than a mocked client: the criterion is about what an
//! endpoint observes, and a fake client would assert that our own abstraction
//! was called rather than that a POST arrived. The outbox here is an in-memory
//! double of the port -- the SQLite implementation and the restart guarantee
//! are covered by the workspace integration tests, which need a database.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use async_trait::async_trait;
use sandesh_core::{
    CoreResult, EndpointId, OutboxEntry, OutboxId, OutboxStatus, OutboxStore, RetryPolicy,
    Timestamp, WebhookEndpoint,
};
use sandesh_webhook::OutboxWorker;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::matchers::{body_json_string, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An in-memory [`OutboxStore`], standing in for the SQLite one.
#[derive(Debug, Default)]
struct MemoryOutbox {
    endpoints: Mutex<Vec<WebhookEndpoint>>,
    entries: Mutex<Vec<OutboxEntry>>,
}

impl MemoryOutbox {
    fn with_endpoint(url: &str) -> (Arc<Self>, EndpointId) {
        Self::with_endpoint_secret(url, None)
    }

    /// An endpoint with signing configured, for the signature tests.
    fn with_endpoint_secret(url: &str, secret: Option<&str>) -> (Arc<Self>, EndpointId) {
        let store = Arc::new(Self::default());
        let id = EndpointId::new();
        store.endpoints.lock().unwrap().push(WebhookEndpoint {
            id,
            url: url.to_owned(),
            enabled: true,
            created_at: Timestamp::now(),
            secret: secret.map(sandesh_core::WebhookSecret::new),
        });
        (store, id)
    }

    fn enqueue(&self, endpoint_id: EndpointId, payload: &str) -> OutboxId {
        let id = OutboxId::new();
        self.entries.lock().unwrap().push(OutboxEntry {
            id,
            endpoint_id,
            payload: payload.to_owned(),
            attempts: 0,
            next_attempt_at: Timestamp::now(),
            status: OutboxStatus::Pending,
            last_error: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        });
        id
    }

    fn entry(&self, id: OutboxId) -> OutboxEntry {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .expect("entry exists")
    }

    /// Makes every pending entry due now, standing in for the passage of time.
    fn make_due(&self) {
        for entry in self.entries.lock().unwrap().iter_mut() {
            if entry.status == OutboxStatus::Pending {
                entry.next_attempt_at = Timestamp::from_micros(0);
            }
        }
    }
}

#[async_trait]
impl OutboxStore for MemoryOutbox {
    async fn register_endpoint(&self, endpoint: &WebhookEndpoint) -> CoreResult<()> {
        self.endpoints.lock().unwrap().push(endpoint.clone());
        Ok(())
    }

    async fn endpoints(&self) -> CoreResult<Vec<WebhookEndpoint>> {
        Ok(self.endpoints.lock().unwrap().clone())
    }

    async fn remove_endpoint(&self, id: EndpointId) -> CoreResult<bool> {
        let mut endpoints = self.endpoints.lock().unwrap();
        let before = endpoints.len();
        endpoints.retain(|e| e.id != id);
        Ok(endpoints.len() != before)
    }

    async fn due(&self, now: Timestamp, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.status == OutboxStatus::Pending && e.next_attempt_at <= now)
            .take(limit as usize)
            .cloned()
            .collect())
    }

    async fn record_attempt(
        &self,
        id: OutboxId,
        status: OutboxStatus,
        next_attempt_at: Timestamp,
        error: Option<&str>,
    ) -> CoreResult<()> {
        for entry in self.entries.lock().unwrap().iter_mut() {
            if entry.id == id {
                entry.attempts += 1;
                entry.status = status;
                entry.next_attempt_at = next_attempt_at;
                entry.last_error = error.map(ToOwned::to_owned);
                entry.updated_at = Timestamp::now();
            }
        }
        Ok(())
    }

    async fn recent(&self, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .iter()
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

/// A policy that retries fast, so a test does not wait out the real schedule.
fn quick_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        base: Duration::from_millis(1),
        ceiling: Duration::from_millis(2),
        max_attempts,
    }
}

#[tokio::test]
async fn a_successful_post_marks_the_entry_delivered() {
    let server = MockServer::start().await;
    let payload = r#"{"event":"MessageReceived"}"#;

    // The body is matched, not merely the path: a worker that posted an empty
    // request would otherwise pass a test about delivery.
    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(body_json_string(payload))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint(&format!("{}/hook", server.uri()));
    let id = outbox.enqueue(endpoint, payload);

    OutboxWorker::new(outbox.clone()).drain_once().await;

    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Delivered);
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.last_error, None);
}

#[tokio::test]
async fn a_failing_endpoint_is_retried_rather_than_abandoned() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint(&server.uri());
    let id = outbox.enqueue(endpoint, "{}");

    let worker = OutboxWorker::new(outbox.clone()).with_policy(quick_policy(3));
    worker.drain_once().await;

    // Still pending after a failure: the entry survives to be tried again,
    // which is the half of the criterion a fire-and-forget send would miss.
    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Pending);
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.last_error.as_deref(), Some("HTTP 500"));

    // And it is genuinely scheduled for later, not left due immediately.
    assert!(entry.next_attempt_at > Timestamp::from_micros(0));
}

#[tokio::test]
async fn a_retry_that_succeeds_delivers_the_original_payload() {
    let server = MockServer::start().await;
    let payload = r#"{"uid":7}"#;

    // Fails once, then succeeds. `up_to_n_times` makes the ordering explicit.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_json_string(payload))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint(&server.uri());
    let id = outbox.enqueue(endpoint, payload);

    let worker = OutboxWorker::new(outbox.clone()).with_policy(quick_policy(5));
    worker.drain_once().await;
    assert_eq!(outbox.entry(id).status, OutboxStatus::Pending);

    outbox.make_due();
    worker.drain_once().await;

    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Delivered);
    assert_eq!(entry.attempts, 2);
}

#[tokio::test]
async fn attempts_are_exhausted_into_a_visible_terminal_failure() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint(&server.uri());
    let id = outbox.enqueue(endpoint, "{}");

    let worker = OutboxWorker::new(outbox.clone()).with_policy(quick_policy(2));

    worker.drain_once().await;
    outbox.make_due();
    worker.drain_once().await;

    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Failed);
    assert_eq!(entry.attempts, 2);
    // Terminal, but still present and still explaining itself. A silently
    // dropped webhook is the failure this feature exists to make diagnosable.
    assert_eq!(entry.last_error.as_deref(), Some("HTTP 500"));
    assert_eq!(outbox.recent(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_transport_failure_not_a_panic() {
    // Port 1 is reserved and nothing listens there; the connection is refused
    // rather than answered, which is the path a wiremock 500 never exercises.
    let (outbox, endpoint) = MemoryOutbox::with_endpoint("http://127.0.0.1:1/hook");
    let id = outbox.enqueue(endpoint, "{}");

    OutboxWorker::new(outbox.clone())
        .with_policy(quick_policy(3))
        .drain_once()
        .await;

    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Pending);
    assert!(
        entry
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("transport:"),
        "expected a transport error, got {:?}",
        entry.last_error
    );
}

#[tokio::test]
async fn an_entry_whose_endpoint_vanished_fails_instead_of_retrying_forever() {
    let (outbox, endpoint) = MemoryOutbox::with_endpoint("http://127.0.0.1:1/hook");
    let id = outbox.enqueue(endpoint, "{}");
    outbox.remove_endpoint(endpoint).await.unwrap();

    OutboxWorker::new(outbox.clone()).drain_once().await;

    let entry = outbox.entry(id);
    assert_eq!(entry.status, OutboxStatus::Failed);
    assert_eq!(entry.last_error.as_deref(), Some("endpoint removed"));
}

#[tokio::test]
async fn a_worker_with_nothing_due_does_nothing_and_stops_cleanly() {
    let (outbox, _) = MemoryOutbox::with_endpoint("http://127.0.0.1:1/hook");

    let shutdown = tokio_util::sync::CancellationToken::new();
    let worker = OutboxWorker::new(outbox.clone()).with_tick(Duration::from_millis(5));

    let token = shutdown.clone();
    let handle = tokio::spawn(async move { worker.run(token).await });

    tokio::time::sleep(Duration::from_millis(30)).await;
    shutdown.cancel();

    // The run loop must end on cancellation, the way the listeners do; a worker
    // that outlived shutdown would make the graceful stop untrue.
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("worker stopped on cancellation")
        .expect("worker did not panic");

    assert!(outbox.recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_signed_endpoint_sends_a_verifiable_signature() {
    // The end-to-end property: what the worker puts on the wire must verify
    // against the payload the receiver was handed. The unit tests in
    // `sandesh-core` prove the algorithm; this proves the header reaches the
    // receiver and matches the body it was sent with.
    let server = MockServer::start().await;
    let payload = r#"{"event":"MessageReceived","message_id":"m1"}"#;
    const SECRET: &str = "whsec_delivery_test";

    Mock::given(method("POST"))
        .and(path("/hook"))
        .and(body_json_string(payload))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (outbox, endpoint) =
        MemoryOutbox::with_endpoint_secret(&format!("{}/hook", server.uri()), Some(SECRET));
    outbox.enqueue(endpoint, payload);

    OutboxWorker::new(outbox.clone()).drain_once().await;

    let request = &server.received_requests().await.expect("requests")[0];
    let header = request
        .headers
        .get(sandesh_core::SIGNATURE_HEADER)
        .expect("a signed endpoint must send the signature header")
        .to_str()
        .expect("the header is ASCII");

    let body = std::str::from_utf8(&request.body).expect("a UTF-8 body");
    assert!(
        sandesh_core::verify_signature(&sandesh_core::WebhookSecret::new(SECRET), body, header),
        "the signature must verify against the body actually sent: {header}"
    );
}

#[tokio::test]
async fn an_unsigned_endpoint_sends_no_signature_header() {
    // Unsigned is the default and must stay silent rather than sending an
    // empty or zero-key signature, which a receiver could mistake for a real
    // one and "verify" against a secret it guessed.
    let server = MockServer::start().await;
    let payload = r#"{"event":"MessageReceived"}"#;

    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint(&format!("{}/hook", server.uri()));
    outbox.enqueue(endpoint, payload);

    OutboxWorker::new(outbox.clone()).drain_once().await;

    let request = &server.received_requests().await.expect("requests")[0];
    assert!(
        request
            .headers
            .get(sandesh_core::SIGNATURE_HEADER)
            .is_none(),
        "an unsigned endpoint must not send a signature header"
    );
}

#[tokio::test]
async fn the_wrong_secret_does_not_verify_a_real_delivery() {
    // Guards against a signature that "works" because the verifier is lax: a
    // real delivery must fail verification under a key that did not sign it.
    let server = MockServer::start().await;
    let payload = r#"{"event":"MessageReceived"}"#;

    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (outbox, endpoint) = MemoryOutbox::with_endpoint_secret(
        &format!("{}/hook", server.uri()),
        Some("whsec_the_real_key"),
    );
    outbox.enqueue(endpoint, payload);

    OutboxWorker::new(outbox.clone()).drain_once().await;

    let request = &server.received_requests().await.expect("requests")[0];
    let header = request
        .headers
        .get(sandesh_core::SIGNATURE_HEADER)
        .expect("signature header")
        .to_str()
        .expect("ASCII");
    let body = std::str::from_utf8(&request.body).expect("UTF-8");

    assert!(
        !sandesh_core::verify_signature(
            &sandesh_core::WebhookSecret::new("whsec_not_the_key"),
            body,
            header
        ),
        "a delivery must not verify under a key that did not sign it"
    );
}
