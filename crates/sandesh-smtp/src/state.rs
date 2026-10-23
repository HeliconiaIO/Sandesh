//! The SMTP session state machine (RFC 5321 §3).
//!
//! Command sequencing is enforced here, in one place, rather than by scattered
//! `if` checks in the command handlers. That matters because sequencing bugs in
//! SMTP are subtle — `RCPT` before `MAIL`, `DATA` with no recipients, a second
//! `MAIL` inside an open transaction — and a single explicit transition
//! function makes each one a visible decision rather than an oversight.
//!
//! ```text
//!  Connected ──EHLO/HELO──▶ Ready ◀─────────────┐
//!      │                      │                  │
//!      │                  MAIL FROM              │ RSET / DATA complete
//!      │                      ▼                  │
//!      │              MailTransaction ───────────┤
//!      │                      │                  │
//!      │                   RCPT TO               │
//!      │                      ▼                  │
//!      │               HasRecipients ────────────┤
//!      │                      │                  │
//!      │                    DATA                 │
//!      │                      ▼                  │
//!      │              ReceivingData ─────────────┘
//!      │
//!      └──QUIT──▶ Closed
//! ```
//!
//! The authentication boundary is deliberate: [`Transition::rejected`] is how
//! `MAIL FROM` will refuse an unauthenticated session once Phase 4.5 lands,
//! and no state needs to be added for it.

use crate::command::Command;

/// Where a session is in the SMTP conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Greeting sent; awaiting `EHLO`/`HELO`.
    Connected,
    /// Greeted; no transaction open.
    Ready,
    /// `MAIL FROM` accepted; awaiting recipients.
    MailTransaction,
    /// At least one recipient accepted; `DATA` is now valid.
    HasRecipients,
    /// Inside `DATA`; content is being collected.
    ReceivingData,
    /// Inside a `BDAT` sequence; chunks are being collected (RFC 3030).
    ///
    /// Distinct from [`ReceivingData`](Self::ReceivingData) because the two
    /// framings are incompatible (ADR-088), and because this state is *between*
    /// chunks: the session is reading commands here, where `ReceivingData` is
    /// reading content until a terminator.
    ReceivingChunks,
    /// The session is finished.
    Closed,
}

impl State {
    /// Whether a transaction is open and would be discarded by a reset.
    #[must_use]
    pub const fn has_open_transaction(self) -> bool {
        matches!(
            self,
            Self::MailTransaction
                | Self::HasRecipients
                | Self::ReceivingData
                | Self::ReceivingChunks
        )
    }

    /// Whether the client has greeted.
    ///
    /// Every transaction command requires this (RFC 5321 §4.1.4).
    #[must_use]
    pub const fn is_greeted(self) -> bool {
        !matches!(self, Self::Connected | Self::Closed)
    }
}

/// What the session should do with a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The command is valid here; move to `next`.
    Accepted {
        /// The state after the command.
        next: State,
    },
    /// The command is valid but the state does not change.
    Stay,
    /// The command is out of sequence; answer `503` with this explanation.
    Rejected {
        /// Why, phrased for the client.
        reason: &'static str,
    },
}

impl Transition {
    /// A rejection with an explanation.
    #[must_use]
    pub const fn rejected(reason: &'static str) -> Self {
        Self::Rejected { reason }
    }

    /// Whether the command was permitted.
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. } | Self::Stay)
    }
}

/// Decides whether `command` is valid in `state`, and what follows.
///
/// This function is the entire sequencing rulebook. It is pure, so the whole
/// matrix in §64 of the phase brief can be tested without a socket.
#[must_use]
pub fn next(state: State, command: &Command) -> Transition {
    match command {
        // Valid in any state (RFC 5321 §4.1.4). QUIT always closes.
        Command::Quit => Transition::Accepted {
            next: State::Closed,
        },
        // NOOP and HELP never affect state. `Unimplemented` joins them: its
        // reply is a 502 chosen by the session, so sequencing does not apply.
        Command::Noop | Command::Help(_) | Command::Unimplemented(_) => Transition::Stay,

        // EHLO/HELO reset any open transaction (RFC 5321 §4.1.4), and RSET does
        // the same by definition — all three land in Ready.
        Command::Ehlo(_) | Command::Helo(_) | Command::Rset => {
            Transition::Accepted { next: State::Ready }
        }

        Command::Mail(_) => match state {
            State::Connected => {
                Transition::rejected("send EHLO or HELO before starting a transaction")
            }
            State::Ready => Transition::Accepted {
                next: State::MailTransaction,
            },
            // RFC 5321 §4.1.1.2: nested MAIL is invalid; the client must RSET.
            // A chunk sequence in progress is just as open as any other
            // transaction, so it refuses for the same reason.
            State::MailTransaction | State::HasRecipients | State::ReceivingChunks => {
                Transition::rejected("a mail transaction is already open; send RSET first")
            }
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        Command::Rcpt(_) => match state {
            State::Connected => Transition::rejected("send EHLO or HELO first"),
            State::Ready => Transition::rejected("send MAIL FROM before RCPT TO"),
            // Additional recipients keep the session in HasRecipients.
            State::MailTransaction | State::HasRecipients => Transition::Accepted {
                next: State::HasRecipients,
            },
            // Once content is arriving the recipient list is fixed: a late
            // RCPT would mean some chunks were accepted before that address
            // was known, which no reply code can honestly describe.
            State::ReceivingChunks => {
                Transition::rejected("RCPT is not permitted once BDAT has begun")
            }
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        Command::Data => match state {
            State::Connected => Transition::rejected("send EHLO or HELO first"),
            State::Ready => Transition::rejected("send MAIL FROM before DATA"),
            // RFC 5321 §4.1.1.4: DATA needs at least one accepted recipient.
            State::MailTransaction => Transition::rejected("no valid recipients given"),
            State::HasRecipients => Transition::Accepted {
                next: State::ReceivingData,
            },
            // RFC 3030 §3: a transaction commits to one content mechanism.
            // Merging the two framings would produce a message neither client
            // meant to send, so the mixture is refused rather than guessed at.
            State::ReceivingChunks => {
                Transition::rejected("DATA is not permitted after BDAT in the same transaction")
            }
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        // BDAT has the same preconditions as DATA — a greeting, a sender and at
        // least one recipient — and additionally continues an existing chunk
        // sequence, which is what makes `ReceivingChunks` an accepting state
        // here where `ReceivingData` is not.
        Command::Bdat(_) => match state {
            State::Connected => Transition::rejected("send EHLO or HELO first"),
            State::Ready => Transition::rejected("send MAIL FROM before BDAT"),
            State::MailTransaction => Transition::rejected("no valid recipients given"),
            State::HasRecipients | State::ReceivingChunks => Transition::Accepted {
                next: State::ReceivingChunks,
            },
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        // RFC 4954 §4: AUTH is valid only after a greeting and only outside a
        // transaction. Allowing it mid-transaction would leave the question of
        // whose credentials the already-accepted recipients were checked
        // against, so it is refused rather than answered.
        Command::Auth(_) => match state {
            State::Connected => Transition::rejected("send EHLO before AUTH"),
            State::Ready => Transition::Stay,
            State::MailTransaction | State::HasRecipients | State::ReceivingChunks => {
                Transition::rejected("AUTH is not permitted during a mail transaction")
            }
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        // RFC 3207 §4: STARTTLS requires a greeting and no open transaction.
        // On success the session resets to Connected, because the handshake
        // discards all prior state — the client must greet again over TLS, and
        // anything learned in cleartext is not trustworthy.
        Command::StartTls => match state {
            State::Connected => Transition::rejected("send EHLO before STARTTLS"),
            State::Ready => Transition::Accepted {
                next: State::Connected,
            },
            State::MailTransaction | State::HasRecipients | State::ReceivingChunks => {
                Transition::rejected("STARTTLS is not permitted during a mail transaction")
            }
            State::ReceivingData | State::Closed => {
                Transition::rejected("command not valid in this state")
            }
        },

        // VRFY and EXPN require a greeting but no transaction.
        Command::Vrfy(_) | Command::Expn(_) => {
            if state.is_greeted() {
                Transition::Stay
            } else {
                Transition::rejected("send EHLO or HELO first")
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::command::{MailCommand, RcptCommand};

    fn mail() -> Command {
        Command::Mail(MailCommand {
            reverse_path: Some(String::from("a@b.test")),
            declared_size: None,
            body_type: None,
        })
    }

    fn rcpt() -> Command {
        Command::Rcpt(RcptCommand {
            forward_path: String::from("c@d.test"),
        })
    }

    #[test]
    fn happy_path_walks_the_machine() {
        let s = State::Connected;
        let Transition::Accepted { next: s } = next(s, &Command::Ehlo(String::from("h"))) else {
            panic!("EHLO must be accepted");
        };
        assert_eq!(s, State::Ready);

        let Transition::Accepted { next: s } = next(s, &mail()) else {
            panic!("MAIL must be accepted");
        };
        assert_eq!(s, State::MailTransaction);

        let Transition::Accepted { next: s } = next(s, &rcpt()) else {
            panic!("RCPT must be accepted");
        };
        assert_eq!(s, State::HasRecipients);

        let Transition::Accepted { next: s } = next(s, &Command::Data) else {
            panic!("DATA must be accepted");
        };
        assert_eq!(s, State::ReceivingData);
    }

    #[test]
    fn additional_recipients_stay_in_has_recipients() {
        let Transition::Accepted { next } = next(State::HasRecipients, &rcpt()) else {
            panic!("a second RCPT must be accepted");
        };
        assert_eq!(next, State::HasRecipients);
    }

    #[test]
    fn sequencing_matrix() {
        // The §64 matrix, expressed as data.
        let cases: Vec<(State, Command, bool, &str)> = vec![
            (State::Connected, rcpt(), false, "RCPT before greeting"),
            (
                State::Connected,
                Command::Data,
                false,
                "DATA before greeting",
            ),
            (State::Connected, mail(), false, "MAIL before greeting"),
            (State::Ready, Command::Data, false, "DATA before MAIL"),
            (State::Ready, rcpt(), false, "RCPT before MAIL"),
            (State::Ready, mail(), true, "MAIL when ready"),
            (State::MailTransaction, rcpt(), true, "first RCPT"),
            (
                State::MailTransaction,
                Command::Data,
                false,
                "DATA with no recipients",
            ),
            (State::MailTransaction, mail(), false, "nested MAIL"),
            (
                State::HasRecipients,
                mail(),
                false,
                "nested MAIL with recipients",
            ),
            (
                State::HasRecipients,
                Command::Data,
                true,
                "DATA with recipients",
            ),
            (State::HasRecipients, rcpt(), true, "additional RCPT"),
            (State::Ready, Command::Rset, true, "RSET when idle"),
            (
                State::HasRecipients,
                Command::Rset,
                true,
                "RSET mid-transaction",
            ),
            (State::Connected, Command::Quit, true, "QUIT any time"),
            (
                State::HasRecipients,
                Command::Quit,
                true,
                "QUIT mid-transaction",
            ),
            (State::Connected, Command::Noop, true, "NOOP any time"),
            (State::Ready, Command::Noop, true, "NOOP when ready"),
            (
                State::Connected,
                Command::Vrfy(String::new()),
                false,
                "VRFY before greeting",
            ),
            (
                State::Ready,
                Command::Vrfy(String::new()),
                true,
                "VRFY when greeted",
            ),
        ];

        for (state, command, should_accept, description) in cases {
            let transition = next(state, &command);
            assert_eq!(
                transition.is_accepted(),
                should_accept,
                "{description}: {state:?} + {command} gave {transition:?}"
            );
        }
    }

    #[test]
    fn quit_always_closes() {
        for state in [
            State::Connected,
            State::Ready,
            State::MailTransaction,
            State::HasRecipients,
        ] {
            assert_eq!(
                next(state, &Command::Quit),
                Transition::Accepted {
                    next: State::Closed
                },
                "QUIT must close from {state:?}"
            );
        }
    }

    #[test]
    fn rset_always_returns_to_ready() {
        for state in [State::Ready, State::MailTransaction, State::HasRecipients] {
            assert_eq!(
                next(state, &Command::Rset),
                Transition::Accepted { next: State::Ready },
                "RSET must reset from {state:?}"
            );
        }
    }

    #[test]
    fn re_greeting_resets_an_open_transaction() {
        // RFC 5321 §4.1.4.
        let transition = next(State::HasRecipients, &Command::Ehlo(String::from("h")));
        assert_eq!(transition, Transition::Accepted { next: State::Ready });
    }

    #[test]
    fn open_transactions_are_identified() {
        assert!(!State::Connected.has_open_transaction());
        assert!(!State::Ready.has_open_transaction());
        assert!(State::MailTransaction.has_open_transaction());
        assert!(State::HasRecipients.has_open_transaction());
        assert!(State::ReceivingData.has_open_transaction());
    }

    #[test]
    fn rejections_explain_themselves() {
        // A 503 with no explanation is useless to whoever is debugging.
        let Transition::Rejected { reason } = next(State::Ready, &rcpt()) else {
            panic!("expected rejection");
        };
        assert!(reason.contains("MAIL FROM"), "unhelpful reason: {reason}");
    }

    #[test]
    fn auth_requires_a_greeting() {
        let auth = Command::Auth(crate::command::AuthCommand {
            mechanism: String::from("PLAIN"),
            initial_response: None,
        });

        assert!(matches!(
            next(State::Connected, &auth),
            Transition::Rejected { .. }
        ));
        assert!(next(State::Ready, &auth).is_accepted());
    }

    #[test]
    fn auth_is_refused_mid_transaction() {
        // Authenticating after recipients were accepted would leave the
        // question of whose credentials they were checked against.
        let auth = Command::Auth(crate::command::AuthCommand {
            mechanism: String::from("PLAIN"),
            initial_response: None,
        });

        for state in [
            State::MailTransaction,
            State::HasRecipients,
            State::ReceivingData,
        ] {
            assert!(
                matches!(next(state, &auth), Transition::Rejected { .. }),
                "AUTH must be refused in {state:?}"
            );
        }
    }

    #[test]
    fn starttls_requires_a_greeting_and_no_transaction() {
        assert!(matches!(
            next(State::Connected, &Command::StartTls),
            Transition::Rejected { .. }
        ));
        assert!(next(State::Ready, &Command::StartTls).is_accepted());

        for state in [
            State::MailTransaction,
            State::HasRecipients,
            State::ReceivingData,
        ] {
            assert!(
                matches!(next(state, &Command::StartTls), Transition::Rejected { .. }),
                "STARTTLS must be refused in {state:?}"
            );
        }
    }

    #[test]
    fn a_successful_starttls_returns_the_session_to_connected() {
        // RFC 3207 §4.2: the handshake discards everything learned in
        // cleartext, so the client must greet again. Landing anywhere other
        // than Connected would leave the pre-TLS EHLO in force.
        assert_eq!(
            next(State::Ready, &Command::StartTls),
            Transition::Accepted {
                next: State::Connected
            }
        );
    }
}
