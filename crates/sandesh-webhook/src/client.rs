//! The outbound HTTP client.
//!
//! Deliberately small. A webhook delivery is one POST of a JSON body with a
//! timeout, and the only thing the worker needs back is whether the endpoint
//! accepted it. Everything richer -- redirects, connection reuse policy,
//! streaming bodies -- is surface that would have to be reasoned about on a
//! path that runs unattended in the background.

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::time::Duration;

/// How long one attempt may take before it counts as a failure.
///
/// A webhook consumer that has not answered in ten seconds is down as far as a
/// test suite is concerned, and the retry schedule exists precisely so that a
/// slow endpoint is retried rather than waited on.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why an attempt did not succeed.
///
/// The distinction that matters is *not* transport-versus-status: both are
/// retried identically, because a 500 and a refused connection are the same
/// fact to a consumer -- the notification did not land. The variants exist so
/// the stored `last_error` says something a developer can act on.
#[derive(Debug)]
pub enum DeliveryError {
    /// The request never completed: DNS, connection refused, timeout.
    Transport(String),
    /// The endpoint answered, but not with success.
    Status(u16),
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(detail) => write!(f, "transport: {detail}"),
            Self::Status(code) => write!(f, "HTTP {code}"),
        }
    }
}

/// Posts webhook payloads.
#[derive(Debug, Clone)]
pub struct WebhookClient {
    http: Client<HttpConnector, Full<Bytes>>,
}

impl Default for WebhookClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookClient {
    /// Builds a client with pooled connections.
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: Client::builder(TokioExecutor::new()).build(HttpConnector::new()),
        }
    }

    /// POSTs `payload` to `url` as JSON.
    ///
    /// Any 2xx is success. Everything else -- including a redirect, which is
    /// not followed -- is a failure the worker will schedule a retry for.
    ///
    /// `signature`, when present, is sent as `X-Sandesh-Signature`. The value
    /// is computed by the caller rather than here: what gets signed is a
    /// domain decision (`sandesh_core::sign_payload`), and a transport that
    /// derived its own would be a second place the wire format could drift.
    ///
    /// # Errors
    /// [`DeliveryError`] if the request failed or the endpoint refused it.
    pub async fn post(
        &self,
        url: &str,
        payload: &str,
        signature: Option<&str>,
    ) -> Result<(), DeliveryError> {
        let mut builder = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(url)
            .header(hyper::header::CONTENT_TYPE, "application/json");

        if let Some(signature) = signature {
            builder = builder.header(sandesh_core::SIGNATURE_HEADER, signature);
        }

        let request = builder
            .body(Full::new(Bytes::from(payload.to_owned())))
            .map_err(|e| DeliveryError::Transport(e.to_string()))?;

        let response = tokio::time::timeout(ATTEMPT_TIMEOUT, self.http.request(request))
            .await
            .map_err(|_| DeliveryError::Transport("timed out".to_owned()))?
            .map_err(|e| DeliveryError::Transport(e.to_string()))?;

        let status = response.status();

        // The body is drained and discarded. Not doing so leaves the connection
        // unusable for the pool, which turns every delivery into a fresh
        // handshake against an endpoint we are likely to post to again.
        let _ = response.into_body().collect().await;

        if status.is_success() {
            Ok(())
        } else {
            Err(DeliveryError::Status(status.as_u16()))
        }
    }
}
