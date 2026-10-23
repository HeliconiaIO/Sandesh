//! IMAP session state machine tests (RFC 9051 §3, phase brief §5 and §24).
//!
//! The table-driven sweep at the bottom is the important one: it asserts a
//! decision for *every* (state, command) pair, so a newly added command cannot
//! quietly inherit permissive behaviour.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_imap::{CommandKind, Refusal, State, Transition, transition};

/// Every state a connection can be in.
const STATES: [State; 4] = [
    State::NotAuthenticated,
    State::Authenticated,
    State::Selected { read_only: false },
    State::Logout,
];

/// Every command the server implements.
const COMMANDS: [CommandKind; 28] = [
    CommandKind::Capability,
    CommandKind::Noop,
    CommandKind::Logout,
    CommandKind::Id,
    CommandKind::Enable,
    CommandKind::StartTls,
    CommandKind::Authenticate,
    CommandKind::Login,
    CommandKind::Select,
    CommandKind::Examine,
    CommandKind::Create,
    CommandKind::Delete,
    CommandKind::Rename,
    CommandKind::Subscribe,
    CommandKind::Unsubscribe,
    CommandKind::List,
    CommandKind::Namespace,
    CommandKind::Status,
    CommandKind::Append,
    CommandKind::Close,
    CommandKind::Unselect,
    CommandKind::Expunge,
    CommandKind::Search,
    CommandKind::Fetch,
    CommandKind::Store,
    CommandKind::Copy,
    CommandKind::Move,
    CommandKind::Check,
];

fn is_rejected(state: State, command: CommandKind) -> bool {
    matches!(transition(state, command), Transition::Rejected { .. })
}

// ------------------------------------------------- any-state commands

#[test]
fn capability_noop_and_id_are_valid_in_every_live_state() {
    // RFC 9051 §6.1: valid in any state, including Not Authenticated.
    for state in [
        State::NotAuthenticated,
        State::Authenticated,
        State::Selected { read_only: false },
        State::Selected { read_only: true },
    ] {
        for command in [CommandKind::Capability, CommandKind::Noop, CommandKind::Id] {
            assert_eq!(
                transition(state, command),
                Transition::Stay,
                "{command} must be valid in {state:?}"
            );
        }
    }
}

#[test]
fn logout_is_always_valid_and_always_terminates() {
    for state in [
        State::NotAuthenticated,
        State::Authenticated,
        State::Selected { read_only: false },
    ] {
        assert_eq!(transition(state, CommandKind::Logout), Transition::Logout);
    }
}

// ------------------------------------------------- the authentication boundary

#[test]
fn authentication_is_required_before_mailbox_commands() {
    // The security-critical rule: an unauthenticated client reaches no mail.
    for command in [
        CommandKind::Select,
        CommandKind::Examine,
        CommandKind::List,
        CommandKind::Status,
        CommandKind::Create,
        CommandKind::Delete,
        CommandKind::Rename,
        CommandKind::Subscribe,
        CommandKind::Unsubscribe,
        CommandKind::Namespace,
        CommandKind::Append,
        CommandKind::Enable,
    ] {
        assert_eq!(
            transition(State::NotAuthenticated, command),
            Transition::Rejected {
                reason: Refusal::NeedsAuthentication
            },
            "{command} must not be reachable before authentication"
        );
    }
}

#[test]
fn message_commands_are_refused_before_authentication() {
    // And they report *authentication*, not "no mailbox selected" — an
    // unauthenticated client should not be told to select a mailbox.
    for command in [
        CommandKind::Fetch,
        CommandKind::Search,
        CommandKind::Store,
        CommandKind::Copy,
        CommandKind::Move,
        CommandKind::Expunge,
        CommandKind::Close,
        CommandKind::Unselect,
        CommandKind::Check,
    ] {
        assert_eq!(
            transition(State::NotAuthenticated, command),
            Transition::Rejected {
                reason: Refusal::NeedsAuthentication
            },
            "{command} must report the authentication requirement"
        );
    }
}

#[test]
fn authenticate_moves_to_authenticated() {
    assert_eq!(
        transition(State::NotAuthenticated, CommandKind::Authenticate),
        Transition::Authenticate
    );
    assert_eq!(
        transition(State::NotAuthenticated, CommandKind::Login),
        Transition::Authenticate
    );
}

#[test]
fn re_authentication_is_refused() {
    // RFC 9051 §6.2: AUTHENTICATE and LOGIN are Not Authenticated commands.
    // Allowing a second one would let a client change identity mid-session
    // while a mailbox from the first identity is still selected.
    for state in [State::Authenticated, State::Selected { read_only: false }] {
        for command in [CommandKind::Authenticate, CommandKind::Login] {
            assert_eq!(
                transition(state, command),
                Transition::Rejected {
                    reason: Refusal::AlreadyAuthenticated
                },
                "{command} must be refused in {state:?}"
            );
        }
    }
}

#[test]
fn starttls_is_refused_once_authenticated() {
    // Renegotiating TLS under an established identity is the confusion
    // RFC 9051 §6.2.1 forbids.
    assert_eq!(
        transition(State::Authenticated, CommandKind::StartTls),
        Transition::Rejected {
            reason: Refusal::AlreadyAuthenticated
        }
    );
    assert_eq!(
        transition(State::NotAuthenticated, CommandKind::StartTls),
        Transition::Stay
    );
}

// ------------------------------------------------- selection

#[test]
fn select_and_examine_differ_only_in_writability() {
    assert_eq!(
        transition(State::Authenticated, CommandKind::Select),
        Transition::Select { read_only: false }
    );
    assert_eq!(
        transition(State::Authenticated, CommandKind::Examine),
        Transition::Select { read_only: true },
        "EXAMINE must open the mailbox read-only (RFC 9051 §6.3.3)"
    );
}

#[test]
fn selecting_while_selected_is_permitted() {
    // RFC 9051 §6.3.2: a SELECT in the Selected state switches mailbox.
    assert_eq!(
        transition(State::Selected { read_only: false }, CommandKind::Select),
        Transition::Select { read_only: false }
    );
}

#[test]
fn message_commands_require_a_selected_mailbox() {
    for command in [
        CommandKind::Fetch,
        CommandKind::Search,
        CommandKind::Store,
        CommandKind::Copy,
        CommandKind::Move,
        CommandKind::Expunge,
    ] {
        assert_eq!(
            transition(State::Authenticated, command),
            Transition::Rejected {
                reason: Refusal::NeedsSelectedMailbox
            },
            "{command} must require a selection"
        );
    }
}

#[test]
fn close_and_unselect_return_to_authenticated() {
    for command in [CommandKind::Close, CommandKind::Unselect] {
        assert_eq!(
            transition(State::Selected { read_only: false }, command),
            Transition::Deselect
        );
    }
}

// ------------------------------------------------- read-only selections

#[test]
fn a_read_only_selection_refuses_mutation() {
    // EXAMINE promises the client that *this* mailbox will not change. STORE,
    // MOVE and EXPUNGE all write to the selected mailbox, so they are refused.
    //
    // COPY is deliberately absent: it writes only to the destination, which is
    // a different mailbox, so RFC 9051 §6.4.7 permits it here. See
    // `a_read_only_selection_still_permits_copying_out`.
    for command in [CommandKind::Store, CommandKind::Move, CommandKind::Expunge] {
        assert_eq!(
            transition(State::Selected { read_only: true }, command),
            Transition::Rejected {
                reason: Refusal::MailboxIsReadOnly
            },
            "{command} must be refused on an EXAMINE-opened mailbox"
        );
    }
}

#[test]
fn a_read_only_selection_still_permits_copying_out() {
    // COPY out of an EXAMINE-opened mailbox is legal (RFC 9051 §6.4.7): the
    // selected mailbox is only read, and the new placement lands in the
    // destination. Refusing it here would make an archiving client unable to
    // file mail it opened read-only.
    assert_eq!(
        transition(State::Selected { read_only: true }, CommandKind::Copy),
        Transition::Stay,
        "COPY does not mutate the selected mailbox"
    );
}

#[test]
fn a_read_only_selection_still_permits_reading() {
    for command in [CommandKind::Fetch, CommandKind::Search] {
        assert_eq!(
            transition(State::Selected { read_only: true }, command),
            Transition::Stay,
            "{command} is a read command and must work under EXAMINE"
        );
    }
}

// ------------------------------------------------- logout

#[test]
fn nothing_is_accepted_after_logout() {
    // Once the connection is finished it accepts no further command — not even
    // NOOP, and not a second LOGOUT.
    for command in COMMANDS {
        assert_eq!(
            transition(State::Logout, command),
            Transition::Rejected {
                reason: Refusal::SessionClosed
            },
            "{command} must be refused after LOGOUT"
        );
    }
}

// ------------------------------------------------- exhaustive sweep

#[test]
fn every_state_and_command_pair_has_a_decision() {
    // Guards against a command being added without its state rules being
    // considered: the assertion is that the pair is decided, and that no
    // command is accepted in a state where it would bypass authentication.
    for state in STATES {
        for command in COMMANDS {
            let outcome = transition(state, command);
            let permitted = !matches!(outcome, Transition::Rejected { .. });

            if permitted && !state.is_authenticated() {
                assert!(
                    matches!(
                        command,
                        CommandKind::Capability
                            | CommandKind::Noop
                            | CommandKind::Id
                            | CommandKind::Logout
                            | CommandKind::StartTls
                            | CommandKind::Authenticate
                            | CommandKind::Login
                    ),
                    "{command} was permitted in {state:?}, which would bypass authentication"
                );
            }
        }
    }
}

#[test]
fn no_command_is_permitted_in_the_logout_state() {
    for command in COMMANDS {
        assert!(is_rejected(State::Logout, command));
    }
}

// ------------------------------------------------- state predicates

#[test]
fn state_predicates_agree_with_the_states() {
    assert!(!State::NotAuthenticated.is_authenticated());
    assert!(State::Authenticated.is_authenticated());
    assert!(State::Selected { read_only: false }.is_authenticated());
    assert!(
        State::Selected { read_only: true }.is_authenticated(),
        "an EXAMINE selection is still an authenticated session"
    );

    assert!(!State::Authenticated.is_selected());
    assert!(State::Selected { read_only: false }.is_selected());

    assert!(State::Selected { read_only: true }.is_read_only());
    assert!(!State::Selected { read_only: false }.is_read_only());
    assert!(
        !State::Authenticated.is_read_only(),
        "with nothing selected there is no mailbox to protect"
    );
}

// --- mailbox command sequencing (phase 5, increment 3) ------------------

/// Every mailbox command, and the state it first becomes legal in.
const MAILBOX_COMMANDS: &[CommandKind] = &[
    CommandKind::Select,
    CommandKind::Examine,
    CommandKind::List,
    CommandKind::Status,
    CommandKind::Create,
    CommandKind::Delete,
    CommandKind::Rename,
    CommandKind::Subscribe,
    CommandKind::Unsubscribe,
];

#[test]
fn no_mailbox_command_is_permitted_before_authentication() {
    for &command in MAILBOX_COMMANDS {
        let outcome = transition(State::NotAuthenticated, command);
        assert!(
            matches!(
                outcome,
                Transition::Rejected {
                    reason: Refusal::NeedsAuthentication
                }
            ),
            "{command} must require authentication, got {outcome:?}"
        );
    }
}

#[test]
fn every_mailbox_command_is_permitted_once_authenticated() {
    for &command in MAILBOX_COMMANDS {
        let outcome = transition(State::Authenticated, command);
        assert!(
            !matches!(outcome, Transition::Rejected { .. }),
            "{command} must be legal when authenticated, got {outcome:?}"
        );
    }
}

#[test]
fn a_read_only_selection_refuses_every_mutating_command() {
    // The invariant STORE and EXPUNGE rely on. "Mutating" means mutating the
    // *selected* mailbox — COPY writes elsewhere and is excluded.
    let read_only = State::Selected { read_only: true };
    for command in [CommandKind::Store, CommandKind::Move, CommandKind::Expunge] {
        assert_eq!(
            transition(read_only, command),
            Transition::Rejected {
                reason: Refusal::MailboxIsReadOnly
            },
            "{command} must be refused on a read-only mailbox"
        );
    }
}

#[test]
fn a_writable_selection_permits_those_same_commands() {
    let writable = State::Selected { read_only: false };
    for command in [
        CommandKind::Store,
        CommandKind::Copy,
        CommandKind::Move,
        CommandKind::Expunge,
    ] {
        assert_eq!(
            transition(writable, command),
            Transition::Stay,
            "{command} must be permitted on a writable mailbox"
        );
    }
}

#[test]
fn mailbox_management_stays_legal_while_a_mailbox_is_selected() {
    // RFC 9051 §6.3: these are Authenticated-or-Selected commands.
    for state in [
        State::Selected { read_only: false },
        State::Selected { read_only: true },
    ] {
        for &command in MAILBOX_COMMANDS {
            assert!(
                !matches!(transition(state, command), Transition::Rejected { .. }),
                "{command} must remain legal in {state:?}"
            );
        }
    }
}

#[test]
fn enable_is_permitted_only_in_the_authenticated_state() {
    // RFC 5161 §3.1 makes `ENABLE` narrower than every other authenticated
    // command: it is legal *only* with no mailbox selected, because enabling an
    // extension can change the syntax of the untagged responses a selected
    // mailbox is already streaming.
    assert_eq!(
        transition(State::Authenticated, CommandKind::Enable),
        Transition::Stay
    );
}

#[test]
fn enable_is_refused_once_a_mailbox_is_selected() {
    // Both selections, because `EXAMINE` is no more permissive here than
    // `SELECT` — the restriction is about response syntax, not about writing.
    for read_only in [false, true] {
        assert_eq!(
            transition(State::Selected { read_only }, CommandKind::Enable),
            Transition::Rejected {
                reason: Refusal::ForbiddenWhileSelected
            },
            "ENABLE must be refused on a selected mailbox (read_only={read_only})"
        );
    }
}

#[test]
fn enable_does_not_bypass_authentication() {
    // The refusal reason matters as much as the refusal: a client told
    // "not permitted while selected" before it has logged in would be
    // misinformed about why, and `ENABLE` must never be a route to acting on a
    // session that has not authenticated.
    assert_eq!(
        transition(State::NotAuthenticated, CommandKind::Enable),
        Transition::Rejected {
            reason: Refusal::NeedsAuthentication
        }
    );
}

#[test]
fn namespace_needs_authentication_but_not_a_selection() {
    // RFC 2342 §5 places `NAMESPACE` in the authenticated state, and a
    // selection neither adds nor removes anything it reports.
    assert_eq!(
        transition(State::Authenticated, CommandKind::Namespace),
        Transition::Stay
    );
    for read_only in [false, true] {
        assert_eq!(
            transition(State::Selected { read_only }, CommandKind::Namespace),
            Transition::Stay
        );
    }
}
