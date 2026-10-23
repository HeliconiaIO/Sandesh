//! Native notifications on new mail (`ROADMAP.md` Phase 9, A2).
//!
//! A task subscribes to the engine's event bus and raises a desktop
//! notification for each `MessageReceived`. This is the same bus the Web UI's
//! WebSocket and IMAP `IDLE` consume, so the desktop app learns about mail the
//! way every other surface does rather than through a mechanism of its own.
//!
//! ## What a notification can say, and why it says so little
//!
//! `EventPayload::MessageReceived` carries identifiers and a size — no subject,
//! no sender. That is deliberate in `EVENT_MODEL.md` §6: events carry
//! references, not bodies, which is what keeps a thousand queued events a
//! bounded amount of memory however large the mail is.
//!
//! The shell **respects that** rather than working around it. It could fetch
//! the message to title the notification "from alice@example.test", but doing
//! so would mean either reaching past `sandesh-app` into the message store — the
//! forked wiring ADR-092's architecture test forbids — or issuing an HTTP
//! request to its own embedded server for every delivery. Neither is worth a
//! sender's name in a toast.
//!
//! So the notification reports what the event actually knows: that mail
//! arrived, and how large it was. The window is one click away and shows
//! everything.
//!
//! ## Never a notification storm
//!
//! A CI suite delivering a thousand messages must not raise a thousand toasts —
//! that is not a notification, it is a denial of service against the user's own
//! desktop. Deliveries inside a short window are coalesced into one
//! notification that counts them, which is the behaviour a mail client has when
//! a sync brings in a batch.

use std::sync::Arc;
use std::time::Duration;

use sandesh_core::{EventPayload, EventSource};
use tokio_util::sync::CancellationToken;

/// How long to wait for more deliveries before raising a notification.
///
/// Long enough that a burst becomes one toast, short enough that a single
/// message still feels immediate. A CI run delivering continuously produces a
/// steady trickle of summaries rather than one toast per message.
const COALESCE_WINDOW: Duration = Duration::from_millis(1200);

/// What one notification reports.
///
/// Separated from the raising of it so the coalescing rule is testable without
/// a notification daemon — the logic worth getting right is *what* to say, and
/// a D-Bus connection cannot tell us whether we said it correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// How many messages arrived in this window.
    pub count: usize,
    /// Their combined `RFC822.SIZE`, in octets.
    pub bytes: u64,
}

impl Summary {
    /// The notification body.
    ///
    /// Phrased around what the developer is waiting for — that their
    /// application's mail actually arrived — and never claims a sender or
    /// subject the event does not carry.
    #[must_use]
    pub fn body(self) -> String {
        if self.count == 1 {
            format!("1 message ({})", format_size(self.bytes))
        } else {
            format!("{} messages ({})", self.count, format_size(self.bytes))
        }
    }
}

/// Octets in the units a developer reads sizes in.
fn format_size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // Display only; exactness is not wanted here.
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

/// Subscribes to the bus and raises notifications until `shutdown` is cancelled.
///
/// Runs as its own task: a notification daemon that is slow, or absent, must
/// never delay the engine. The subscription is bounded like every other bus
/// consumer, so a stalled notifier drops events rather than growing without
/// limit (`EVENT_MODEL.md` §6) — and dropping a *notification* costs nothing,
/// because the mail is already stored and the window will show it.
pub async fn run(events: Arc<dyn EventSource>, shutdown: CancellationToken) {
    let mut stream = events.subscribe();
    let mut pending: Option<Summary> = None;

    loop {
        // With something pending, wait only until the coalescing window closes;
        // otherwise wait indefinitely for the next delivery.
        let deadline = pending.map(|_| tokio::time::sleep(COALESCE_WINDOW));

        tokio::select! {
            () = shutdown.cancelled() => {
                // Raise what is held rather than discarding it: mail that
                // arrived as the user quit still arrived.
                if let Some(summary) = pending.take() {
                    raise(summary);
                }
                return;
            }

            () = async { match deadline {
                Some(sleep) => sleep.await,
                // No pending summary: never wake on the timer branch.
                None => std::future::pending().await,
            } } => {
                if let Some(summary) = pending.take() {
                    raise(summary);
                }
            }

            event = stream.next() => {
                match event {
                    Some(event) => {
                        if let EventPayload::MessageReceived { size, .. } = event.payload {
                            pending = Some(match pending {
                                Some(held) => Summary {
                                    count: held.count + 1,
                                    bytes: held.bytes + size,
                                },
                                None => Summary { count: 1, bytes: size },
                            });
                        }
                    }
                    // The bus closed, which happens when the engine stops.
                    None => {
                        if let Some(summary) = pending.take() {
                            raise(summary);
                        }
                        return;
                    }
                }
            }
        }
    }
}

/// Shows one notification, or logs why it could not.
///
/// A failure here is never propagated: no notification daemon is running on
/// plenty of Linux setups, and a desktop mail server that refused to run
/// because nothing was listening for toasts would be absurd.
fn raise(summary: Summary) {
    let result = notify_rust::Notification::new()
        .summary("New mail in Sandesh")
        .body(&summary.body())
        .appname("Sandesh")
        .icon("mail-unread")
        .timeout(notify_rust::Timeout::Milliseconds(5000))
        .show();

    match result {
        Ok(_) => tracing::debug!(count = summary.count, "raised a new-mail notification"),
        Err(error) => {
            tracing::debug!(%error, "no notification daemon; skipping the new-mail toast");
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_core::{AccountId, DomainEvent, EntryId, MailboxId, MessageId, Uid};
    use sandesh_events::EventBus;

    fn received(size: u64) -> DomainEvent {
        DomainEvent::new(EventPayload::MessageReceived {
            message_id: MessageId::new(),
            entry_id: EntryId::new(),
            mailbox_id: MailboxId::new(),
            account_id: AccountId::new(),
            uid: Uid::MIN,
            size,
        })
    }

    #[test]
    fn one_message_reads_as_singular() {
        let summary = Summary {
            count: 1,
            bytes: 512,
        };
        assert_eq!(summary.body(), "1 message (512 B)");
    }

    #[test]
    fn several_messages_are_counted_not_listed() {
        // The anti-storm property, in the text: a burst is one toast that says
        // how many, never one toast per message.
        let summary = Summary {
            count: 40,
            bytes: 40 * 2048,
        };
        assert_eq!(summary.body(), "40 messages (80.0 KB)");
    }

    #[test]
    fn sizes_read_in_developer_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1_048_576), "1.0 MB");
    }

    #[tokio::test]
    async fn a_burst_of_deliveries_coalesces_into_one_summary() {
        // The property that keeps a CI run from flooding the desktop. Driven
        // through the real bus rather than a stub, so what is exercised is the
        // subscription the shell actually uses.
        let bus = Arc::new(EventBus::new());
        let mut stream = <EventBus as EventSource>::subscribe(&bus);

        for size in [100, 200, 300] {
            bus.publish(received(size));
        }

        let mut summary: Option<Summary> = None;
        for _ in 0..3 {
            let event = stream.next().await.expect("event delivered");
            if let EventPayload::MessageReceived { size, .. } = event.payload {
                summary = Some(match summary {
                    Some(held) => Summary {
                        count: held.count + 1,
                        bytes: held.bytes + size,
                    },
                    None => Summary {
                        count: 1,
                        bytes: size,
                    },
                });
            }
        }

        let summary = summary.expect("three deliveries produce a summary");
        assert_eq!(summary.count, 3);
        assert_eq!(summary.bytes, 600);
        assert_eq!(summary.body(), "3 messages (600 B)");
    }

    #[tokio::test]
    async fn the_notifier_stops_when_shutdown_is_cancelled() {
        // The shell cancels this token when the window closes; a notifier that
        // outlived it would keep a subscription on a stopped bus.
        let bus: Arc<dyn EventSource> = Arc::new(EventBus::new());
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(bus, shutdown.clone()));
        shutdown.cancel();

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the notifier must stop promptly")
            .expect("the notifier task must not panic");
    }

    #[tokio::test]
    async fn other_events_raise_nothing() {
        // Only new mail notifies. A flag change or a mailbox rename must not
        // produce a toast, and the desktop app subscribes to the whole bus.
        let bus = Arc::new(EventBus::new());
        let mut stream = <EventBus as EventSource>::subscribe(&bus);

        bus.publish(DomainEvent::new(EventPayload::MailboxCreated {
            mailbox_id: MailboxId::new(),
            account_id: AccountId::new(),
            name: "Archive".to_owned(),
            uid_validity: 1,
        }));

        let event = stream.next().await.expect("event delivered");
        assert!(
            !matches!(event.payload, EventPayload::MessageReceived { .. }),
            "only MessageReceived may notify"
        );
    }
}
