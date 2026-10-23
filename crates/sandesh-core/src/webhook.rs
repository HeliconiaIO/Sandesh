//! Outbound notifications: endpoints, the durable outbox, and the retry policy.
//!
//! Phase 8 A2 pushes events *outward* for the first time. Everything before it
//! waited to be asked — a client fetches, subscribes or scrapes. A webhook
//! reverses that, which is why the acceptance criterion is about durability
//! rather than about the request: "webhooks retry on failure **and survive a
//! restart**".
//!
//! # What lives here and what does not
//!
//! The types, the state machine and the backoff schedule are domain logic and
//! live here. The HTTP client, the worker task and the SQL live in
//! `sandesh-webhook` and `sandesh-storage` respectively (ADR-085): the core has
//! no runtime and no transport, which `tests/architecture.rs` enforces
//! mechanically. [`RetryPolicy::schedule`] is the whole retry decision, and it
//! is a pure function of the attempt count — so it is unit-testable without a
//! socket, a clock or a database, and the worker cannot quietly disagree with
//! it.
//!
//! # Why an entry is per placement
//!
//! `MessageReceived` is emitted once per placement, so a message delivered to
//! three accounts produces three events and three outbox entries. Collapsing
//! them into one notification would invent an aggregate event the bus does not
//! have, and force a consumer that cares about one account to re-derive what it
//! had already been told.

use crate::{EndpointId, OutboxId, Timestamp};
use std::time::Duration;

/// A registered destination for outbound notifications.
///
/// Endpoints persist across restarts, unlike the fault rules of ADR-084. The
/// reasoning there does not transfer: a forgotten fault makes a server look
/// broken, whereas a forgotten endpoint is inert — and an outbox entry that
/// survived a restart pointing at an endpoint that did not would be incoherent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEndpoint {
    /// Identity.
    pub id: EndpointId,
    /// Where notifications are `POST`ed.
    pub url: String,
    /// Whether the worker should enqueue for this endpoint.
    ///
    /// Disabling stops *new* entries; it does not cancel pending ones, which
    /// are already-accepted work. A developer silencing a noisy endpoint is
    /// asking about the future, not asking to lose what is queued.
    pub enabled: bool,
    /// When it was registered.
    pub created_at: Timestamp,
    /// The shared secret payloads are signed with, if signing is enabled.
    ///
    /// `None` means unsigned, which is the default and not a degraded state:
    /// an endpoint pointed at a local test receiver has nothing to spoof.
    pub secret: Option<WebhookSecret>,
}

/// Where an outbox entry has got to.
///
/// Three states, and the terminal pair is the point. An entry that exhausted
/// its attempts becomes [`Failed`](Self::Failed) and **stays visible**: a
/// webhook that stopped being delivered without saying so is precisely the
/// opacity the Phase 7 diagnostics work existed to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    /// Not yet delivered; the worker will attempt it when due.
    Pending,
    /// Accepted by the endpoint.
    Delivered,
    /// Attempts exhausted. Terminal, and deliberately not deleted.
    Failed,
}

impl OutboxStatus {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }

    /// Parses the stored spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(Self::Pending),
            "delivered" => Some(Self::Delivered),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Whether no further attempt will be made.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed)
    }
}

/// One queued notification: a payload bound for one endpoint.
///
/// The payload is rendered at enqueue time rather than at send time. The
/// message it describes can be deleted before the worker runs, and a webhook
/// that silently stopped carrying data because its subject was cleaned up
/// would be a retry that was never really retrying the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    /// Identity.
    pub id: OutboxId,
    /// The destination.
    pub endpoint_id: EndpointId,
    /// The JSON body to POST, built when the entry was enqueued.
    pub payload: String,
    /// How many attempts have been made.
    pub attempts: u32,
    /// When the next attempt becomes due.
    pub next_attempt_at: Timestamp,
    /// Where it has got to.
    pub status: OutboxStatus,
    /// Why the most recent attempt failed, if one did.
    pub last_error: Option<String>,
    /// When it was enqueued.
    pub created_at: Timestamp,
    /// When it last changed.
    pub updated_at: Timestamp,
}

/// How long to wait before each retry, and when to give up.
///
/// Exponential with a ceiling. The ceiling matters more than the growth: an
/// endpoint that is down is usually down for a while, and an unbounded doubling
/// would push the next attempt hours out, long past the lifetime of the CI job
/// that is watching for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Delay before the first retry.
    pub base: Duration,
    /// The longest any backoff may grow to.
    pub ceiling: Duration,
    /// Total attempts allowed, including the first.
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    /// Defaults chosen for a test suite watching, not for a production bus.
    ///
    /// Five attempts over roughly half a minute: long enough that a restarting
    /// endpoint is caught, short enough that a test asserting terminal failure
    /// does not have to wait out a production-shaped schedule.
    fn default() -> Self {
        Self {
            base: Duration::from_secs(2),
            ceiling: Duration::from_secs(16),
            max_attempts: 5,
        }
    }
}

/// What the worker should do after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Try again after this delay.
    After(Duration),
    /// Attempts are exhausted; mark the entry [`OutboxStatus::Failed`].
    GiveUp,
}

impl RetryPolicy {
    /// Decides what follows a failed attempt.
    ///
    /// `attempts` is the count **including** the one that just failed, which is
    /// the number the storage row already holds after the worker increments it.
    /// Taking it in that form means the worker never has to reason about
    /// off-by-one between the row and the policy.
    ///
    /// The delay doubles per attempt and saturates at [`ceiling`](Self::ceiling).
    /// Saturating arithmetic is deliberate: a large `attempts` must clamp, not
    /// wrap into a short delay that would hammer a struggling endpoint.
    #[must_use]
    pub fn schedule(&self, attempts: u32) -> Retry {
        if attempts >= self.max_attempts {
            return Retry::GiveUp;
        }

        let shift = attempts.saturating_sub(1).min(31);
        let factor = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
        let delay = self
            .base
            .checked_mul(factor)
            .unwrap_or(self.ceiling)
            .min(self.ceiling);

        Retry::After(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_the_base() {
        let policy = RetryPolicy {
            base: Duration::from_secs(1),
            ceiling: Duration::from_mins(1),
            max_attempts: 10,
        };

        assert_eq!(policy.schedule(1), Retry::After(Duration::from_secs(1)));
        assert_eq!(policy.schedule(2), Retry::After(Duration::from_secs(2)));
        assert_eq!(policy.schedule(3), Retry::After(Duration::from_secs(4)));
        assert_eq!(policy.schedule(4), Retry::After(Duration::from_secs(8)));
    }

    #[test]
    fn backoff_saturates_at_the_ceiling() {
        let policy = RetryPolicy {
            base: Duration::from_secs(1),
            ceiling: Duration::from_secs(5),
            max_attempts: 20,
        };

        // Doubling would reach 8s here; the ceiling is the whole point.
        assert_eq!(policy.schedule(5), Retry::After(Duration::from_secs(5)));
        assert_eq!(policy.schedule(15), Retry::After(Duration::from_secs(5)));
    }

    #[test]
    fn a_huge_attempt_count_clamps_rather_than_wrapping() {
        // The shift is the hazard: without the clamp, `1 << 32` is UB-adjacent
        // and a wrapped factor would produce a *short* delay for the most
        // troubled endpoint — the exact inversion of what backoff is for.
        let policy = RetryPolicy {
            base: Duration::from_secs(1),
            ceiling: Duration::from_secs(30),
            max_attempts: u32::MAX,
        };

        assert_eq!(
            policy.schedule(u32::MAX - 1),
            Retry::After(Duration::from_secs(30))
        );
    }

    #[test]
    fn attempts_are_exhausted_at_the_maximum() {
        let policy = RetryPolicy {
            base: Duration::from_secs(1),
            ceiling: Duration::from_secs(8),
            max_attempts: 3,
        };

        assert_eq!(policy.schedule(2), Retry::After(Duration::from_secs(2)));
        assert_eq!(policy.schedule(3), Retry::GiveUp);
        assert_eq!(policy.schedule(4), Retry::GiveUp);
    }

    #[test]
    fn status_round_trips_through_its_stored_spelling() {
        for status in [
            OutboxStatus::Pending,
            OutboxStatus::Delivered,
            OutboxStatus::Failed,
        ] {
            assert_eq!(OutboxStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(OutboxStatus::parse("in-flight"), None);
    }

    #[test]
    fn only_pending_is_non_terminal() {
        assert!(!OutboxStatus::Pending.is_terminal());
        assert!(OutboxStatus::Delivered.is_terminal());
        assert!(OutboxStatus::Failed.is_terminal());
    }
}

/// The shared secret a webhook payload is signed with.
///
/// A newtype rather than a `String`, for the same reason
/// `sandesh_proto::Secret` exists: `Debug` and `Display` render `[redacted]`,
/// so the key cannot reach a log through a struct someone derived without
/// thinking about it. Reading it requires [`WebhookSecret::expose`], which is
/// deliberately greppable.
///
/// It lives here rather than being reused from `sandesh-proto` because
/// `sandesh-webhook` may depend on `sandesh-core` and nothing else in the
/// project (ADR-085, asserted by `tests/architecture.rs`), and the worker is
/// what needs to sign.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WebhookSecret(String);

impl WebhookSecret {
    /// Wraps a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the secret. Every call site is a place the key is used.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty, which signs nothing useful.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl std::fmt::Display for WebhookSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The header carrying a payload signature.
///
/// # Verifying a Sandesh webhook
///
/// The value is `t=<unix seconds>,v1=<hex>`, and the signed material is
/// `"<t>.<raw body>"` — the body exactly as received, before any JSON parsing
/// and re-serialisation, which would change the bytes.
///
/// ```python
/// import hmac, hashlib, time
///
/// def verify(secret: str, body: bytes, header: str, tolerance=300) -> bool:
///     fields = dict(f.split("=", 1) for f in header.split(","))
///     t = int(fields["t"])
///     if abs(time.time() - t) > tolerance:   # reject a replayed capture
///         return False
///     expected = hmac.new(
///         secret.encode(), f"{t}.".encode() + body, hashlib.sha256
///     ).hexdigest()
///     return hmac.compare_digest(expected, fields["v1"])
/// ```
///
/// Compare in constant time (`hmac.compare_digest` above): a byte-by-byte
/// comparison leaks, through timing, how much of a forged signature was right.
pub const SIGNATURE_HEADER: &str = "X-Sandesh-Signature";

/// Builds the `X-Sandesh-Signature` value for a payload.
///
/// The format is `t=<unix seconds>,v1=<hex>`, and the signed material is
/// `"<t>.<body>"` — the timestamp is *inside* what is signed, not merely
/// alongside it. Signing the body alone would let anyone who captured one
/// delivery replay it verbatim forever; binding the timestamp lets a receiver
/// reject anything outside its tolerated clock skew, and a receiver that does
/// not care can simply ignore `t`.
///
/// `v1` names the scheme so a second one can be added later without a
/// receiver having to guess which it is looking at — the same reason the
/// event stream spells out its frame kinds.
#[must_use]
pub fn sign_payload(secret: &WebhookSecret, payload: &str, timestamp_secs: i64) -> String {
    let digest = signature_hex(secret, payload, timestamp_secs);
    format!("t={timestamp_secs},v1={digest}")
}

/// The raw hex digest over `"<timestamp>.<payload>"`.
///
/// Separated from [`sign_payload`] so a verifier — a test, or a consumer
/// written against this crate — can recompute the digest without parsing the
/// header it is checking.
#[must_use]
pub fn signature_hex(secret: &WebhookSecret, payload: &str, timestamp_secs: i64) -> String {
    use hmac::{KeyInit, Mac, SimpleHmac};

    // `new_from_slice` accepts any key length for HMAC, so this cannot fail.
    let Ok(mut mac) = SimpleHmac::<sha2::Sha256>::new_from_slice(secret.expose().as_bytes()) else {
        unreachable!("HMAC accepts keys of any length")
    };
    mac.update(timestamp_secs.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Whether `header` is a valid signature for `payload` under `secret`.
///
/// Provided so a consumer verifying Sandesh's webhooks has a reference
/// implementation that cannot disagree with the signer, and so the tests
/// assert the pair rather than a hard-coded digest.
///
/// Comparison is constant-time: a byte-by-byte `==` on a digest leaks, through
/// timing, how much of a forged signature was correct.
#[must_use]
pub fn verify_signature(secret: &WebhookSecret, payload: &str, header: &str) -> bool {
    use subtle::ConstantTimeEq;

    let mut timestamp = None;
    let mut provided = None;
    for field in header.split(',') {
        match field.split_once('=') {
            Some(("t", value)) => timestamp = value.parse::<i64>().ok(),
            Some(("v1", value)) => provided = Some(value),
            _ => {}
        }
    }

    let (Some(timestamp), Some(provided)) = (timestamp, provided) else {
        return false;
    };

    let expected = signature_hex(secret, payload, timestamp);
    expected.as_bytes().ct_eq(provided.as_bytes()).unwrap_u8() == 1
}

#[cfg(test)]
mod signing_tests {
    use super::*;

    const PAYLOAD: &str = r#"{"event":"MessageReceived","message_id":"abc"}"#;

    fn secret() -> WebhookSecret {
        WebhookSecret::new("whsec_test_key")
    }

    #[test]
    fn a_signature_verifies_against_its_own_payload() {
        let header = sign_payload(&secret(), PAYLOAD, 1_700_000_000);
        assert!(verify_signature(&secret(), PAYLOAD, &header));
    }

    #[test]
    fn the_header_has_the_documented_shape() {
        let header = sign_payload(&secret(), PAYLOAD, 1_700_000_000);

        assert!(header.starts_with("t=1700000000,v1="));
        let digest = header.split("v1=").nth(1).unwrap_or_default();
        // SHA-256 as hex: the length is what a receiver validates before
        // spending time on a comparison.
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_digest_matches_an_independent_implementation() {
        // A signer that only ever agrees with its own verifier can be
        // consistently wrong and every round-trip test above would still pass.
        // This vector was produced by Python's `hmac` module, which is what a
        // consumer is most likely to verify with:
        //
        //   hmac.new(b"whsec_test_key",
        //            b"1700000000." + payload,
        //            hashlib.sha256).hexdigest()
        //
        // If this fails, the wire format changed and every existing receiver
        // breaks — that is the point of pinning it.
        assert_eq!(
            signature_hex(&secret(), PAYLOAD, 1_700_000_000),
            "3c3524388606587ad04b5e307f57c5a646cbf761383d8c81ca12aedf4365dc46"
        );
    }

    #[test]
    fn a_changed_payload_does_not_verify() {
        let header = sign_payload(&secret(), PAYLOAD, 1_700_000_000);
        // One byte, in the middle of a JSON value: the case a signature exists
        // to catch is tampering, not corruption.
        let tampered = PAYLOAD.replace("abc", "abd");

        assert!(!verify_signature(&secret(), &tampered, &header));
    }

    #[test]
    fn a_different_secret_does_not_verify() {
        let header = sign_payload(&secret(), PAYLOAD, 1_700_000_000);
        let attacker = WebhookSecret::new("whsec_wrong_key");

        assert!(!verify_signature(&attacker, PAYLOAD, &header));
    }

    #[test]
    fn the_timestamp_is_inside_the_signed_material() {
        // The property replay protection rests on: moving `t` must invalidate
        // the signature. If the body alone were signed, a captured delivery
        // could be replayed forever by rewriting the timestamp.
        let header = sign_payload(&secret(), PAYLOAD, 1_700_000_000);
        let moved = header.replace("t=1700000000", "t=1700009999");

        assert!(!verify_signature(&secret(), PAYLOAD, &moved));
    }

    #[test]
    fn the_same_input_signs_identically() {
        // Deterministic, so a receiver can recompute rather than store, and so
        // a retry of the same entry is verifiable the same way as the first
        // attempt.
        let first = sign_payload(&secret(), PAYLOAD, 1_700_000_000);
        let second = sign_payload(&secret(), PAYLOAD, 1_700_000_000);

        assert_eq!(first, second);
    }

    #[test]
    fn a_malformed_header_is_rejected_rather_than_panicking() {
        // These arrive from whatever a consumer sends back, so none of them may
        // be a crash: a verifier is reachable from untrusted input.
        for header in [
            "",
            "garbage",
            "t=notanumber,v1=abc",
            "v1=abc",              // no timestamp
            "t=1700000000",        // no digest
            "t=1700000000,v1=",    // empty digest
            "t=1700000000,v2=abc", // a scheme this build does not know
        ] {
            assert!(
                !verify_signature(&secret(), PAYLOAD, header),
                "`{header}` must not verify"
            );
        }
    }

    #[test]
    fn a_secret_never_renders_itself() {
        // The reason this is a newtype. A `Debug` on any struct holding one
        // must not be able to write the key to a log.
        let secret = WebhookSecret::new("whsec_do_not_log_me");

        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert!(!format!("{secret:?}{secret}").contains("do_not_log_me"));
        // And the endpoint that carries one must not leak it either.
        let endpoint = WebhookEndpoint {
            id: crate::EndpointId::new(),
            url: "https://example.test/hook".to_owned(),
            enabled: true,
            created_at: Timestamp::now(),
            secret: Some(secret),
        };
        assert!(!format!("{endpoint:?}").contains("do_not_log_me"));
    }
}
