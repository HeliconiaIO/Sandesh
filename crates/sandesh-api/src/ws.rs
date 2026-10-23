//! The `WebSocket` event stream (`API_DESIGN.md` §5).
//!
//! This module is a **transport**. It decides nothing about mail: not which
//! mailbox a message belongs to, not how flags work, not how UIDs are
//! allocated. It subscribes to [`EventBus`], turns each [`DomainEvent`] into a
//! JSON frame, and writes it to a socket. Every judgement about what happened
//! was made in the core before the event existed.
//!
//! ## Why the wire format is written out by hand
//!
//! The frames below are a public contract that a UI and CI scripts will depend
//! on. Deriving them from the domain enum would mean a Rust variant rename
//! silently breaks every client, so kind names are produced by
//! [`EventKind::as_str`](sandesh_core::EventKind::as_str) — already asserted
//! stable by a core test — and payload fields are written explicitly.
//!
//! ## Backpressure
//!
//! Each connection owns one bounded [`Subscriber`]. If a client stops reading,
//! its queue fills, the bus drops the oldest events for that subscriber alone,
//! and the connection is sent `{"type":"lagged","dropped":N}` and expected to
//! refetch. No other client is affected, and — the property that matters — the
//! producer is never blocked, so a stalled browser tab cannot slow SMTP.
//!
//! ## Lifetime
//!
//! The subscription lives in the connection task and is dropped with it. A
//! client that disappears without a close frame is detected by the send
//! failing; either way the task ends and the queue is freed.

use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use sandesh_core::{DomainEvent, EventPayload};
use sandesh_events::{EventBus, RecvError, Subscriber};
use serde::Deserialize;
use serde_json::{Value, json};

/// How long to wait for a client's next frame before sending a keepalive ping.
const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// What a client may ask for.
///
/// Unknown message types are answered with an error frame rather than closing:
/// a client from a newer UI build talking to an older server should degrade,
/// not drop.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    /// Narrow the stream. Sending it again replaces the previous filter.
    Subscribe {
        /// Optional server-side filter.
        #[serde(default)]
        filters: Filters,
    },
    /// Return to receiving everything.
    Unsubscribe,
    /// Liveness check; answered with `pong`.
    Ping,
    /// Replay events missed while disconnected (ADR-100).
    ///
    /// `after_seq` is a **`bus_seq`**, not the per-connection `seq`: it is the
    /// only identifier stable across a reconnect, which is exactly when resume
    /// is used.
    Resume {
        /// The last `bus_seq` the client durably processed.
        after_seq: u64,
    },
}

/// The server-side filter (`API_DESIGN.md` §5).
///
/// Deliberately a fixed set of fields rather than an expression language: a
/// client must not be able to hand the server an arbitrary predicate to run.
///
/// Filtering happens here, before serialisation, so a UI watching one mailbox
/// does not receive and discard every other mailbox's traffic.
#[derive(Debug, Default, Clone, Deserialize)]
struct Filters {
    /// Only events concerning this account.
    #[serde(default)]
    account_id: Option<String>,
    /// Only events concerning this mailbox.
    #[serde(default)]
    mailbox_id: Option<String>,
    /// Only these event kinds, by wire name.
    #[serde(default)]
    events: Option<Vec<String>>,
}

impl Filters {
    /// Whether an event should reach this client.
    fn accepts(&self, event: &DomainEvent) -> bool {
        if let Some(kinds) = &self.events
            && !kinds.iter().any(|k| k == event.kind().as_str())
        {
            return false;
        }
        if let Some(wanted) = &self.mailbox_id
            && !mailbox_ids(&event.payload).iter().any(|id| id == wanted)
        {
            return false;
        }
        if let Some(wanted) = &self.account_id
            && account_id(&event.payload).as_deref() != Some(wanted.as_str())
        {
            return false;
        }
        true
    }
}

/// Every mailbox an event concerns.
///
/// A move concerns two, and a message deletion concerns all of them, so a
/// client filtered to either endpoint of a move still sees it.
fn mailbox_ids(payload: &EventPayload) -> Vec<String> {
    match payload {
        EventPayload::MessageReceived { mailbox_id, .. }
        | EventPayload::EntryCreated { mailbox_id, .. }
        | EventPayload::EntryDeleted { mailbox_id, .. }
        | EventPayload::EntryFlagsChanged { mailbox_id, .. }
        | EventPayload::MailboxCreated { mailbox_id, .. }
        | EventPayload::MailboxDeleted { mailbox_id, .. }
        | EventPayload::MailboxRenamed { mailbox_id, .. }
        | EventPayload::MailboxSubscriptionChanged { mailbox_id, .. } => {
            vec![mailbox_id.to_string()]
        }
        EventPayload::EntryMoved {
            from_mailbox_id,
            to_mailbox_id,
            ..
        } => vec![from_mailbox_id.to_string(), to_mailbox_id.to_string()],
        EventPayload::MessageDeleted { mailbox_ids, .. } => {
            mailbox_ids.iter().map(ToString::to_string).collect()
        }
        EventPayload::AccountCreated { .. }
        | EventPayload::AccountDeleted { .. }
        | EventPayload::DeliveryFailed { .. }
        | EventPayload::WorkspaceCreated { .. }
        | EventPayload::WorkspaceDeleted { .. }
        | EventPayload::WorkspaceSwitched { .. }
        | EventPayload::WorkspacePurged { .. } => Vec::new(),
    }
}

/// The account an event concerns, where the payload names one.
fn account_id(payload: &EventPayload) -> Option<String> {
    match payload {
        EventPayload::MessageReceived { account_id, .. }
        | EventPayload::MailboxCreated { account_id, .. }
        | EventPayload::MailboxDeleted { account_id, .. }
        | EventPayload::AccountCreated { account_id, .. }
        | EventPayload::AccountDeleted { account_id, .. } => Some(account_id.to_string()),
        _ => None,
    }
}

/// Renders an event payload as its wire object.
///
/// Identifiers and minimal context only — never message content
/// (`EVENT_MODEL.md` §1). A 20 MB attachment must not become a 20 MB frame, and
/// a client that wants the message fetches it.
///
/// It is one long match by design: this function *is* the payload wire
/// contract, and splitting it up would scatter that contract across several
/// places where a field could be changed without anyone seeing the whole shape.
#[allow(clippy::too_many_lines)]
fn payload_json(payload: &EventPayload) -> Value {
    match payload {
        EventPayload::MessageReceived {
            message_id,
            entry_id,
            mailbox_id,
            account_id,
            uid,
            size,
        } => json!({
            "message_id": message_id.to_string(),
            "entry_id": entry_id.to_string(),
            "mailbox_id": mailbox_id.to_string(),
            "account_id": account_id.to_string(),
            "uid": uid.get(),
            "size": size,
        }),
        EventPayload::MessageDeleted {
            message_id,
            mailbox_ids,
        } => json!({
            "message_id": message_id.to_string(),
            "mailbox_ids": mailbox_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        }),
        EventPayload::EntryCreated {
            entry_id,
            message_id,
            mailbox_id,
            uid,
        }
        | EventPayload::EntryDeleted {
            entry_id,
            message_id,
            mailbox_id,
            uid,
        } => json!({
            "entry_id": entry_id.to_string(),
            "message_id": message_id.to_string(),
            "mailbox_id": mailbox_id.to_string(),
            "uid": uid.get(),
        }),
        // `mod_seq` is deliberately not surfaced to the web client: it is an
        // IMAP synchronisation detail (RFC 7162) with no meaning to a UI that
        // sees live events already.
        EventPayload::EntryFlagsChanged {
            entry_id,
            mailbox_id,
            uid,
            added,
            removed,
            flags_now,
            ..
        } => json!({
            "entry_id": entry_id.to_string(),
            "mailbox_id": mailbox_id.to_string(),
            "uid": uid.get(),
            "added": added,
            "removed": removed,
            "flags_now": flags_now,
        }),
        EventPayload::EntryMoved {
            entry_id,
            message_id,
            from_mailbox_id,
            to_mailbox_id,
            old_uid,
            new_uid,
        } => json!({
            "entry_id": entry_id.to_string(),
            "message_id": message_id.to_string(),
            "from_mailbox_id": from_mailbox_id.to_string(),
            "to_mailbox_id": to_mailbox_id.to_string(),
            "old_uid": old_uid.get(),
            "new_uid": new_uid.get(),
        }),
        EventPayload::MailboxCreated {
            mailbox_id,
            account_id,
            name,
            uid_validity,
        } => json!({
            "mailbox_id": mailbox_id.to_string(),
            "account_id": account_id.to_string(),
            "name": name,
            "uid_validity": uid_validity,
        }),
        EventPayload::MailboxDeleted {
            mailbox_id,
            account_id,
            name,
        } => json!({
            "mailbox_id": mailbox_id.to_string(),
            "account_id": account_id.to_string(),
            "name": name,
        }),
        EventPayload::MailboxRenamed {
            mailbox_id,
            old_name,
            new_name,
        } => json!({
            "mailbox_id": mailbox_id.to_string(),
            "old_name": old_name,
            "new_name": new_name,
        }),
        EventPayload::MailboxSubscriptionChanged {
            mailbox_id,
            subscribed,
        } => json!({
            "mailbox_id": mailbox_id.to_string(),
            "subscribed": subscribed,
        }),
        EventPayload::AccountCreated {
            account_id,
            username,
        }
        | EventPayload::AccountDeleted {
            account_id,
            username,
        } => json!({
            "account_id": account_id.to_string(),
            "username": username,
        }),
        EventPayload::DeliveryFailed {
            delivery_id,
            reason,
        } => json!({
            "delivery_id": delivery_id.to_string(),
            "reason": reason,
        }),
        EventPayload::WorkspaceCreated { workspace_id, name } => json!({
            "workspace_id": workspace_id,
            "name": name,
        }),
        EventPayload::WorkspaceDeleted { workspace_id } => json!({
            "workspace_id": workspace_id,
        }),
        EventPayload::WorkspaceSwitched { previous, current } => json!({
            "previous": previous,
            "current": current,
        }),
        EventPayload::WorkspacePurged {
            workspace_id,
            deleted_messages,
            deleted_traces,
            reclaimed_bytes,
        } => json!({
            "workspace_id": workspace_id,
            "deleted_messages": deleted_messages,
            "deleted_traces": deleted_traces,
            "reclaimed_bytes": reclaimed_bytes,
        }),
    }
}

/// Renders a full event frame.
///
/// `seq` is the connection's own counter, not the bus's: a client counts the
/// frames *it* received, so a gap means this connection missed something
/// (`API_DESIGN.md` §5). The bus's total order is reported separately as
/// `bus_seq` for correlating across connections.
pub(crate) fn event_frame(event: &DomainEvent, connection_seq: u64) -> Value {
    json!({
        "type": "event",
        "seq": connection_seq,
        "bus_seq": event.seq,
        "event": {
            "kind": event.kind().as_str(),
            "id": event.id.to_string(),
            "occurred_at": rfc3339(event.occurred_at),
            "payload": payload_json(&event.payload),
        }
    })
}

/// RFC 3339 rendering, matching the REST convention.
fn rfc3339(timestamp: sandesh_core::Timestamp) -> String {
    timestamp
        .to_datetime()
        .and_then(|dt| {
            dt.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| String::from("1970-01-01T00:00:00Z"))
}

/// Serves one `WebSocket` connection until it closes.
///
/// Returns when the client goes away, the send fails, or the bus closes; the
/// [`Subscriber`] is dropped on every path, so no queue is leaked.
pub async fn serve_connection(socket: WebSocket, bus: Arc<EventBus>) {
    let subscriber = bus.subscribe();
    run(socket, subscriber, Some(bus)).await;
}

/// The connection loop, split out so tests can drive it with any subscriber.
///
/// `bus` is optional so tests can drive the loop with a bare subscriber; a
/// connection without it answers `resume` with an error rather than silence.
async fn run(socket: WebSocket, mut subscriber: Subscriber, bus: Option<Arc<EventBus>>) {
    let (mut sink, mut stream) = socket.split();
    let mut filters = Filters::default();
    let mut connection_seq: u64 = 0;
    let mut ping = tokio::time::interval(PING_INTERVAL);
    // The first tick fires immediately; skip it so a client is not pinged the
    // instant it connects.
    ping.tick().await;

    loop {
        tokio::select! {
            // Biased so that a client's close frame is observed promptly even
            // under a heavy event stream, rather than being starved by it.
            biased;

            incoming = stream.next() => {
                match incoming {
                    Some(Ok(WsMessage::Text(text))) => {
                        // Resume replays through the same filter and counter as
                        // live traffic, so a resumed client cannot tell the
                        // difference apart from the `replayed` marker.
                        if let Some(after_seq) = parse_resume(&text)
                            && let Some(bus) = bus.as_ref()
                        {
                            let replayed = replay_frames(
                                bus,
                                after_seq,
                                &filters,
                                &mut connection_seq,
                            );
                            let mut failed = false;
                            for frame in replayed {
                                if send(&mut sink, &frame).await.is_err() {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed {
                                break;
                            }
                            continue;
                        }
                        let reply = handle_client_frame(&text, &mut filters);
                        if send(&mut sink, &reply).await.is_err() {
                            break;
                        }
                    }
                    // A close frame, a closed stream, and a read error all mean
                    // the same thing: this client is finished.
                    Some(Ok(WsMessage::Close(_)) | Err(_)) | None => break,
                    // Pong and binary frames need no action.
                    Some(Ok(_)) => {}
                }
            }

            received = subscriber.recv() => {
                match received {
                    Ok(event) => {
                        if !filters.accepts(&event) {
                            continue;
                        }
                        connection_seq += 1;
                        if send(&mut sink, &event_frame(&event, connection_seq)).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(dropped)) => {
                        // The client is behind. Tell it how far so it can
                        // resynchronise; the connection stays usable.
                        tracing::debug!(dropped, "websocket subscriber lagged");
                        let frame = json!({"type": "lagged", "dropped": dropped});
                        if send(&mut sink, &frame).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }

            _ = ping.tick() => {
                if sink.send(WsMessage::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }

    // Dropping `subscriber` here releases the bus queue. Closing is best-effort:
    // the peer may already be gone, which is not an error worth reporting.
    let _ = sink.close().await;
}

/// Handles one client frame, returning the reply to send.
///
/// Every frame is answered, including a malformed one: silence would leave a
/// client unable to tell a rejected request from a lost one.
fn handle_client_frame(text: &str, filters: &mut Filters) -> Value {
    match serde_json::from_str::<ClientFrame>(text) {
        Ok(ClientFrame::Subscribe { filters: wanted }) => {
            *filters = wanted;
            json!({"type": "subscribed"})
        }
        Ok(ClientFrame::Unsubscribe) => {
            *filters = Filters::default();
            json!({"type": "unsubscribed"})
        }
        Ok(ClientFrame::Ping) => json!({"type": "pong"}),
        // Resume needs the bus and the connection's own counter, so the loop
        // handles it; reaching here means the caller did not route it.
        Ok(ClientFrame::Resume { .. }) => json!({
            "type": "error",
            "message": "resume is not available on this connection",
        }),
        // Malformed input from a client is that client's problem, not a reason
        // to drop a connection that may still be useful.
        Err(error) => json!({
            "type": "error",
            "message": format!("unrecognised frame: {error}"),
        }),
    }
}

/// Returns the `after_seq` of a `resume` frame, or `None` for anything else.
///
/// A malformed `resume` returns `None` so it falls through to the ordinary
/// handler and is answered with the same error any bad frame gets.
fn parse_resume(text: &str) -> Option<u64> {
    match serde_json::from_str::<ClientFrame>(text) {
        Ok(ClientFrame::Resume { after_seq }) => Some(after_seq),
        _ => None,
    }
}

/// Builds the frames answering a `resume`.
///
/// Always begins with a `resumed` frame stating what the client is about to
/// receive, so a client that gets nothing else knows it was already current
/// rather than being left to guess from silence. A gap yields a single
/// `resume_gap` frame instead: the client must refetch, and replaying a
/// partial history would let it believe otherwise.
fn replay_frames(
    bus: &EventBus,
    after_seq: u64,
    filters: &Filters,
    connection_seq: &mut u64,
) -> Vec<Value> {
    match bus.replay_since(after_seq) {
        Ok(events) => {
            let matched: Vec<&sandesh_core::DomainEvent> =
                events.iter().filter(|e| filters.accepts(e)).collect();
            let mut frames = Vec::with_capacity(matched.len() + 1);
            frames.push(json!({
                "type": "resumed",
                "after_seq": after_seq,
                "count": matched.len(),
            }));
            for event in matched {
                *connection_seq += 1;
                let mut frame = event_frame(event, *connection_seq);
                // Mark replay so a client can tell history from live traffic.
                if let Some(object) = frame.as_object_mut() {
                    object.insert(String::from("replayed"), Value::Bool(true));
                }
                frames.push(frame);
            }
            frames
        }
        Err(gap) => {
            tracing::debug!(after_seq, %gap, "websocket resume fell outside the replay window");
            vec![json!({
                "type": "resume_gap",
                "after_seq": after_seq,
                "oldest_seq": gap.oldest_seq,
                "message": gap.to_string(),
            })]
        }
    }
}

/// Sends a JSON frame.
async fn send<S>(sink: &mut S, value: &Value) -> Result<(), ()>
where
    S: SinkExt<WsMessage> + Unpin,
{
    // A value built by `json!` from owned data cannot fail to serialise; if it
    // somehow did, dropping the frame beats killing the connection.
    let Ok(text) = serde_json::to_string(value) else {
        tracing::error!("failed to serialise a websocket frame");
        return Ok(());
    };
    sink.send(WsMessage::Text(text.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_core::{AccountId, EntryId, MailboxId, MessageId, Uid};
    use sandesh_events::DEFAULT_CAPACITY;

    fn received(mailbox: MailboxId, account: AccountId) -> DomainEvent {
        DomainEvent::new(EventPayload::MessageReceived {
            message_id: MessageId::new(),
            entry_id: EntryId::new(),
            mailbox_id: mailbox,
            account_id: account,
            uid: Uid::MIN,
            size: 1024,
        })
    }

    #[test]
    fn resume_replays_only_what_the_client_missed() {
        let bus = EventBus::new();
        let mailbox = MailboxId::new();
        let account = AccountId::new();
        for _ in 0..3 {
            bus.publish(received(mailbox, account));
        }

        let mut connection_seq = 0;
        let frames = replay_frames(&bus, 1, &Filters::default(), &mut connection_seq);

        assert_eq!(frames[0]["type"], "resumed");
        assert_eq!(frames[0]["count"], 2);
        assert_eq!(
            frames.len(),
            3,
            "the header frame plus the two missed events"
        );
        // bus_seq is what a client resumes on, so it must be the real one.
        assert_eq!(frames[1]["bus_seq"], 2);
        assert_eq!(frames[2]["bus_seq"], 3);
        assert_eq!(frames[1]["replayed"], true);
    }

    #[test]
    fn a_current_client_resumes_with_no_events() {
        let bus = EventBus::new();
        bus.publish(received(MailboxId::new(), AccountId::new()));

        let mut connection_seq = 0;
        let frames = replay_frames(&bus, 1, &Filters::default(), &mut connection_seq);

        // Already current: a header saying "nothing missed", not a gap.
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "resumed");
        assert_eq!(frames[0]["count"], 0);
    }

    #[test]
    fn resume_past_the_window_reports_a_gap_rather_than_partial_history() {
        let bus = EventBus::with_capacities(DEFAULT_CAPACITY, 2);
        let mailbox = MailboxId::new();
        let account = AccountId::new();
        for _ in 0..3 {
            bus.publish(received(mailbox, account));
        }

        let mut connection_seq = 0;
        let frames = replay_frames(&bus, 0, &Filters::default(), &mut connection_seq);

        // One frame, and it must not carry events: a client told "here is some
        // of what you missed" would wrongly believe it is caught up.
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "resume_gap");
        assert_eq!(frames[0]["oldest_seq"], 2);
    }

    #[test]
    fn resume_respects_the_active_filter() {
        let bus = EventBus::new();
        let watched = MailboxId::new();
        let account = AccountId::new();
        bus.publish(received(watched, account));
        bus.publish(received(MailboxId::new(), account));

        let filters = Filters {
            mailbox_id: Some(watched.to_string()),
            ..Filters::default()
        };
        let mut connection_seq = 0;
        let frames = replay_frames(&bus, 0, &filters, &mut connection_seq);

        assert_eq!(frames[0]["count"], 1, "the other mailbox must not replay");
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn resume_continues_the_connection_sequence() {
        let bus = EventBus::new();
        let mailbox = MailboxId::new();
        let account = AccountId::new();
        bus.publish(received(mailbox, account));
        bus.publish(received(mailbox, account));

        // A connection that already delivered 5 frames keeps counting from 5.
        let mut connection_seq = 5;
        let frames = replay_frames(&bus, 0, &Filters::default(), &mut connection_seq);

        assert_eq!(frames[1]["seq"], 6);
        assert_eq!(frames[2]["seq"], 7);
        assert_eq!(connection_seq, 7);
    }

    #[test]
    fn a_resume_frame_parses_and_other_frames_do_not() {
        assert_eq!(
            parse_resume(r#"{"type":"resume","after_seq":42}"#),
            Some(42)
        );
        assert_eq!(parse_resume(r#"{"type":"ping"}"#), None);
        // Malformed resume falls through to the ordinary error reply.
        assert_eq!(parse_resume(r#"{"type":"resume"}"#), None);
    }

    #[test]
    fn an_event_frame_has_the_documented_shape() {
        let event = received(MailboxId::new(), AccountId::new());
        let frame = event_frame(&event, 7);

        assert_eq!(frame["type"], "event");
        assert_eq!(frame["seq"], 7);
        assert_eq!(frame["event"]["kind"], "MessageReceived");
        assert!(frame["event"]["payload"]["message_id"].is_string());
        assert!(
            frame["event"]["occurred_at"]
                .as_str()
                .is_some_and(|t| t.ends_with('Z'))
        );
    }

    #[test]
    fn kind_names_are_the_stable_wire_strings() {
        // Not the Rust variant name by luck — the core's own stable mapping.
        let event = received(MailboxId::new(), AccountId::new());
        assert_eq!(event_frame(&event, 1)["event"]["kind"], "MessageReceived");
    }

    #[test]
    fn frames_never_carry_message_content() {
        // EVENT_MODEL §1: identifiers only, so frame size is independent of
        // message size.
        let event = received(MailboxId::new(), AccountId::new());
        let rendered = event_frame(&event, 1).to_string();
        for forbidden in ["body", "raw", "content", "subject"] {
            assert!(!rendered.contains(forbidden), "{forbidden} leaked");
        }
    }

    #[test]
    fn an_empty_filter_accepts_everything() {
        let filters = Filters::default();
        assert!(filters.accepts(&received(MailboxId::new(), AccountId::new())));
    }

    #[test]
    fn a_mailbox_filter_excludes_other_mailboxes() {
        let wanted = MailboxId::new();
        let filters = Filters {
            mailbox_id: Some(wanted.to_string()),
            ..Filters::default()
        };

        assert!(filters.accepts(&received(wanted, AccountId::new())));
        assert!(!filters.accepts(&received(MailboxId::new(), AccountId::new())));
    }

    #[test]
    fn a_kind_filter_excludes_other_kinds() {
        let filters = Filters {
            events: Some(vec![String::from("EntryFlagsChanged")]),
            ..Filters::default()
        };
        assert!(!filters.accepts(&received(MailboxId::new(), AccountId::new())));

        let flags = DomainEvent::new(EventPayload::EntryFlagsChanged {
            entry_id: EntryId::new(),
            mailbox_id: MailboxId::new(),
            uid: Uid::MIN,
            added: vec![String::from("\\Seen")],
            removed: Vec::new(),
            flags_now: vec![String::from("\\Seen")],
            mod_seq: sandesh_core::ModSeq::MIN,
        });
        assert!(filters.accepts(&flags));
    }

    #[test]
    fn an_account_filter_excludes_other_accounts() {
        let wanted = AccountId::new();
        let filters = Filters {
            account_id: Some(wanted.to_string()),
            ..Filters::default()
        };
        assert!(filters.accepts(&received(MailboxId::new(), wanted)));
        assert!(!filters.accepts(&received(MailboxId::new(), AccountId::new())));
    }

    #[test]
    fn a_move_matches_both_of_its_mailboxes() {
        // A client watching either end of a move must see it.
        let from = MailboxId::new();
        let to = MailboxId::new();
        let event = DomainEvent::new(EventPayload::EntryMoved {
            entry_id: EntryId::new(),
            message_id: MessageId::new(),
            from_mailbox_id: from,
            to_mailbox_id: to,
            old_uid: Uid::MIN,
            new_uid: Uid::MIN,
        });

        for watched in [from, to] {
            let filters = Filters {
                mailbox_id: Some(watched.to_string()),
                ..Filters::default()
            };
            assert!(filters.accepts(&event));
        }
    }

    #[test]
    fn subscribe_replaces_the_filter() {
        let mut filters = Filters::default();
        let mailbox = MailboxId::new();

        let reply = handle_client_frame(
            &format!(r#"{{"type":"subscribe","filters":{{"mailbox_id":"{mailbox}"}}}}"#),
            &mut filters,
        );
        assert_eq!(reply["type"], "subscribed");
        assert!(!filters.accepts(&received(MailboxId::new(), AccountId::new())));

        let reply = handle_client_frame(r#"{"type":"unsubscribe"}"#, &mut filters);
        assert_eq!(reply["type"], "unsubscribed");
        assert!(filters.accepts(&received(MailboxId::new(), AccountId::new())));
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let mut filters = Filters::default();
        let reply = handle_client_frame(r#"{"type":"ping"}"#, &mut filters);
        assert_eq!(reply["type"], "pong");
    }

    #[test]
    fn a_malformed_frame_is_answered_not_fatal() {
        // A bad frame must not take down a connection that may still be useful.
        let mut filters = Filters::default();
        for bad in ["not json", r#"{"type":"teleport"}"#, "{}"] {
            let reply = handle_client_frame(bad, &mut filters);
            assert_eq!(reply["type"], "error", "for input {bad}");
        }
    }

    #[test]
    fn a_client_cannot_supply_an_arbitrary_predicate() {
        // Filters are a fixed set of fields; anything else is ignored rather
        // than interpreted.
        let mut filters = Filters::default();
        let reply = handle_client_frame(
            r#"{"type":"subscribe","filters":{"sql":"1=1","script":"drop"}}"#,
            &mut filters,
        );
        assert_eq!(reply["type"], "subscribed");
        assert!(filters.accepts(&received(MailboxId::new(), AccountId::new())));
    }
}
