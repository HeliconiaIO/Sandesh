//! The `IMAP4rev2` session state machine (RFC 9051 §3).
//!
//! IMAP has four connection states, and almost every command is legal in only
//! some of them. RFC 9051 §3 defines them; this module is the single place that
//! decides whether a command may run, mirroring the approach
//! `sandesh-smtp::state` takes for RFC 5321.
//!
//! ```text
//!                    ┌────────────────────┐
//!   greeting ───────▶│ Not Authenticated  │
//!   (untagged OK)    └─────────┬──────────┘
//!                              │ AUTHENTICATE (ok)
//!   greeting ────┐             ▼
//!   (PREAUTH)    │   ┌────────────────────┐
//!                └──▶│   Authenticated    │◀──┐
//!                    └─────────┬──────────┘   │ CLOSE / UNSELECT,
//!                              │ SELECT /     │ or a failed SELECT
//!                              │ EXAMINE      │
//!                              ▼              │
//!                    ┌────────────────────┐   │
//!                    │      Selected      │───┘
//!                    └─────────┬──────────┘
//!                              │ LOGOUT, or fatal error
//!                              ▼
//!                    ┌────────────────────┐
//!                    │      Logout        │
//!                    └────────────────────┘
//! ```
//!
//! # Why a type and not two booleans
//!
//! An `authenticated: bool` plus a `selected: Option<Mailbox>` admits the
//! nonsensical state "selected but not authenticated". Making the states an
//! enum means that combination cannot be represented, so it cannot be reached
//! by a bug (RFC 9051 §3 requires exactly this discipline, and the phase brief
//! §5 asks for invalid transitions to be impossible rather than merely
//! unlikely).
//!
//! # A failed SELECT is a state change
//!
//! RFC 9051 §6.3.2 is explicit and easy to get wrong: if a `SELECT` fails, the
//! session does **not** keep its previous mailbox — it returns to
//! Authenticated. [`Transition::Deselect`] exists to make that visible.

use crate::command::CommandKind;

/// Which RFC 9051 §3 state a connection is in.
///
/// `Selected` deliberately carries no mailbox data: the state machine decides
/// *whether* a command is allowed, and giving it the selected mailbox's
/// contents would make it depend on storage. The session owns that data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Connected, no valid credentials yet (RFC 9051 §3.1).
    NotAuthenticated,
    /// Authenticated, but no mailbox selected (RFC 9051 §3.2).
    Authenticated,
    /// A mailbox is selected and its messages are accessible (RFC 9051 §3.3).
    ///
    /// The `read_only` flag distinguishes `EXAMINE` from `SELECT`: both reach
    /// this state, but a read-only selection must refuse mutations
    /// (RFC 9051 §6.3.3).
    Selected {
        /// Whether the mailbox was opened with `EXAMINE`.
        read_only: bool,
    },
    /// The connection is finished (RFC 9051 §3.4). No command is valid.
    Logout,
}

impl State {
    /// Whether the client has authenticated.
    #[must_use]
    pub const fn is_authenticated(self) -> bool {
        matches!(self, Self::Authenticated | Self::Selected { .. })
    }

    /// Whether a mailbox is currently selected.
    #[must_use]
    pub const fn is_selected(self) -> bool {
        matches!(self, Self::Selected { .. })
    }

    /// Whether the selected mailbox refuses mutation.
    ///
    /// False when nothing is selected: there is no mailbox to protect, and the
    /// caller will have rejected the command for want of a selection anyway.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Selected { read_only: true })
    }
}

/// Why a command was refused in the current state.
///
/// Separate variants because RFC 9051 §6 wants different responses: a command
/// that is unknown everywhere is `BAD`, whereas a known command used in the
/// wrong state is `NO` with an explanation the user can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The command requires authentication (RFC 9051 §6.3).
    NeedsAuthentication,
    /// The command requires a selected mailbox (RFC 9051 §6.4).
    NeedsSelectedMailbox,
    /// Already authenticated; re-authenticating is forbidden (RFC 9051 §6.2).
    AlreadyAuthenticated,
    /// The selected mailbox is read-only (RFC 9051 §6.3.3, `EXAMINE`).
    MailboxIsReadOnly,
    /// The command is legal only with no mailbox selected
    /// (RFC 5161 §3.1, `ENABLE`).
    ///
    /// Distinct from [`Self::NeedsSelectedMailbox`], which is its mirror
    /// image. `ENABLE` is the one command this server implements that is
    /// *narrower* than "authenticated or selected": RFC 5161 §3.1 requires a
    /// client to issue it before selecting, because enabling an extension can
    /// change the syntax of the untagged responses a selected mailbox is
    /// already streaming.
    ForbiddenWhileSelected,
    /// The session has logged out.
    SessionClosed,
}

impl Refusal {
    /// The human-readable text accompanying the tagged response.
    ///
    /// Deliberately non-specific about *why* authentication failed elsewhere;
    /// these texts describe sequencing only and leak nothing about accounts.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NeedsAuthentication => "command requires authentication",
            Self::NeedsSelectedMailbox => "command requires a selected mailbox",
            Self::AlreadyAuthenticated => "already authenticated",
            Self::MailboxIsReadOnly => "mailbox is read-only",
            Self::ForbiddenWhileSelected => "command is not permitted while a mailbox is selected",
            Self::SessionClosed => "session is closing",
        }
    }
}

/// What the session should do with a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Valid here, and the state does not change.
    Stay,
    /// Valid, and the session becomes authenticated.
    Authenticate,
    /// Valid, and a mailbox becomes selected.
    Select {
        /// Whether the selection is read-only (`EXAMINE`).
        read_only: bool,
    },
    /// Valid, and the session returns to Authenticated with nothing selected.
    ///
    /// Used by `CLOSE`/`UNSELECT`, and by a `SELECT` that failed.
    Deselect,
    /// Valid, and the connection terminates.
    Logout,
    /// Not valid here.
    Rejected {
        /// Why, so the session can phrase the response.
        reason: Refusal,
    },
}

/// Decides whether `command` may run while in `state`.
///
/// This is the whole authority on command sequencing. The exhaustive `match` is
/// intentional: adding a command to [`CommandKind`] without deciding its state
/// rules becomes a compile error rather than an accidental permit.
#[must_use]
pub fn transition(state: State, command: CommandKind) -> Transition {
    use CommandKind as C;

    // A logged-out connection accepts nothing further (RFC 9051 §3.4).
    if state == State::Logout {
        return Transition::Rejected {
            reason: Refusal::SessionClosed,
        };
    }

    match command {
        // --- Valid in every state (RFC 9051 §6.1) ---
        C::Capability | C::Noop | C::Id => Transition::Stay,
        C::Logout => Transition::Logout,

        // --- Not Authenticated only (RFC 9051 §6.2) ---
        C::Authenticate | C::Login => {
            if state.is_authenticated() {
                Transition::Rejected {
                    reason: Refusal::AlreadyAuthenticated,
                }
            } else {
                Transition::Authenticate
            }
        }
        // STARTTLS is refused once authenticated: renegotiating under an
        // established identity is the confusion RFC 9051 §6.2.1 forbids.
        C::StartTls => {
            if state.is_authenticated() {
                Transition::Rejected {
                    reason: Refusal::AlreadyAuthenticated,
                }
            } else {
                Transition::Stay
            }
        }

        // `ENABLE` (RFC 5161 §3.1) is Authenticated-state *only*. It is not
        // grouped with the commands below because "authenticated or selected"
        // would be wrong in both directions: it must be refused before login,
        // and refused again once a mailbox is selected.
        C::Enable => {
            if !state.is_authenticated() {
                Transition::Rejected {
                    reason: Refusal::NeedsAuthentication,
                }
            } else if matches!(state, State::Selected { .. }) {
                Transition::Rejected {
                    reason: Refusal::ForbiddenWhileSelected,
                }
            } else {
                Transition::Stay
            }
        }

        // --- Authenticated or Selected (RFC 9051 §6.3) ---
        C::Select => require_auth(state, Transition::Select { read_only: false }),
        C::Examine => require_auth(state, Transition::Select { read_only: true }),
        //
        // `IDLE` (RFC 2177 §3) belongs here too: it is legal in Authenticated
        // *and* Selected, and it mutates nothing, so an `EXAMINE`d mailbox may
        // be idled on exactly as a `SELECT`ed one may. Whether a session has
        // anything to report is the handler's concern — an Authenticated-state
        // IDLE simply has no mailbox to report about.
        C::Create
        | C::Delete
        | C::Rename
        | C::Subscribe
        | C::Unsubscribe
        | C::List
        | C::Namespace
        | C::Status
        | C::Append
        | C::Idle => require_auth(state, Transition::Stay),

        // --- Selected only (RFC 9051 §6.4) ---
        C::Close | C::Unselect => require_selected(state, Transition::Deselect),

        // `COPY` is grouped with the readers, not the writers: it mutates the
        // *destination*, never the selected mailbox, so RFC 9051 §6.4.7 permits
        // it on an `EXAMINE`d selection. The destination's own writability is
        // the handler's check, because only the handler knows which mailbox
        // that is. `MOVE` is not here — it removes from the source.
        // `CHECK` joins the readers for the same reason `COPY` does: RFC 9051
        // §6.4.1 requires a selection but defines no change to it, so an
        // `EXAMINE`d (read-only) selection may issue it.
        C::Fetch | C::Search | C::Copy | C::Check => require_selected(state, Transition::Stay),

        // Mutating commands additionally require a writable selection.
        C::Store | C::Move | C::Expunge => match require_selected(state, Transition::Stay) {
            Transition::Stay if state.is_read_only() => Transition::Rejected {
                reason: Refusal::MailboxIsReadOnly,
            },
            other => other,
        },
    }
}

/// Permits `allowed` only when authenticated.
const fn require_auth(state: State, allowed: Transition) -> Transition {
    if state.is_authenticated() {
        allowed
    } else {
        Transition::Rejected {
            reason: Refusal::NeedsAuthentication,
        }
    }
}

/// Permits `allowed` only when a mailbox is selected.
///
/// Reports the *most informative* refusal: an unauthenticated client is told to
/// authenticate, not to select a mailbox it could not select anyway.
const fn require_selected(state: State, allowed: Transition) -> Transition {
    if state.is_selected() {
        allowed
    } else if state.is_authenticated() {
        Transition::Rejected {
            reason: Refusal::NeedsSelectedMailbox,
        }
    } else {
        Transition::Rejected {
            reason: Refusal::NeedsAuthentication,
        }
    }
}
