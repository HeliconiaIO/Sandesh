//! POP3 session state machine (RFC 1939 §3).

use crate::command::Pop3Command;

/// The state of a POP3 connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop3State {
    /// AUTHORIZATION state: awaiting identification/credentials (RFC 1939 §7).
    Authorization {
        /// Username provided by a preceding `USER` command.
        user: Option<String>,
    },
    /// TRANSACTION state: authenticated; mailbox snapshot active (RFC 1939 §5).
    Transaction,
    /// UPDATE state: session ending; deleting marked messages (RFC 1939 §6).
    Update,
}

impl Default for Pop3State {
    fn default() -> Self {
        Self::Authorization { user: None }
    }
}

impl Pop3State {
    /// Checks whether the given command is permitted in the current state.
    #[must_use]
    pub fn is_command_permitted(&self, cmd: &Pop3Command) -> bool {
        match self {
            Self::Authorization { .. } => matches!(
                cmd,
                Pop3Command::User(_)
                    | Pop3Command::Pass(_)
                    | Pop3Command::Quit
                    | Pop3Command::Noop
                    | Pop3Command::Capa
                    | Pop3Command::Stls
                    | Pop3Command::Auth { .. }
            ),
            Self::Transaction => matches!(
                cmd,
                Pop3Command::Stat
                    | Pop3Command::List(_)
                    | Pop3Command::Retr(_)
                    | Pop3Command::Dele(_)
                    | Pop3Command::Noop
                    | Pop3Command::Rset
                    | Pop3Command::Quit
                    | Pop3Command::Top(_, _)
                    | Pop3Command::Uidl(_)
                    | Pop3Command::Capa
            ),
            Self::Update => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_permitted_commands() {
        let auth = Pop3State::Authorization { user: None };
        assert!(auth.is_command_permitted(&Pop3Command::User("a".into())));
        assert!(auth.is_command_permitted(&Pop3Command::Pass("p".into())));
        assert!(auth.is_command_permitted(&Pop3Command::Quit));
        assert!(auth.is_command_permitted(&Pop3Command::Capa));
        assert!(auth.is_command_permitted(&Pop3Command::Noop));
        assert!(auth.is_command_permitted(&Pop3Command::Stls));

        // Transaction commands are rejected in Authorization state
        assert!(!auth.is_command_permitted(&Pop3Command::Stat));
        assert!(!auth.is_command_permitted(&Pop3Command::List(None)));
        assert!(!auth.is_command_permitted(&Pop3Command::Retr(1)));
        assert!(!auth.is_command_permitted(&Pop3Command::Dele(1)));
        assert!(!auth.is_command_permitted(&Pop3Command::Rset));
        assert!(!auth.is_command_permitted(&Pop3Command::Top(1, 0)));
        assert!(!auth.is_command_permitted(&Pop3Command::Uidl(None)));
    }

    #[test]
    fn transaction_permitted_commands() {
        let trans = Pop3State::Transaction;
        assert!(trans.is_command_permitted(&Pop3Command::Stat));
        assert!(trans.is_command_permitted(&Pop3Command::List(None)));
        assert!(trans.is_command_permitted(&Pop3Command::Retr(1)));
        assert!(trans.is_command_permitted(&Pop3Command::Dele(1)));
        assert!(trans.is_command_permitted(&Pop3Command::Noop));
        assert!(trans.is_command_permitted(&Pop3Command::Rset));
        assert!(trans.is_command_permitted(&Pop3Command::Top(1, 5)));
        assert!(trans.is_command_permitted(&Pop3Command::Uidl(None)));
        assert!(trans.is_command_permitted(&Pop3Command::Capa));
        assert!(trans.is_command_permitted(&Pop3Command::Quit));

        // Auth commands are rejected in Transaction state
        assert!(!trans.is_command_permitted(&Pop3Command::User("a".into())));
        assert!(!trans.is_command_permitted(&Pop3Command::Pass("p".into())));
        assert!(!trans.is_command_permitted(&Pop3Command::Stls));
    }
}
