//! POP3 protocol commands (RFC 1939, RFC 2449, RFC 2595, RFC 5034).
//!
//! POP3 commands are sent as single-line ASCII strings terminated by CRLF.
//! Command verbs are case-insensitive. Arguments are space-separated.

use std::fmt;

/// A parsed POP3 client command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop3Command {
    /// `USER <username>` — specifies the user mailbox (RFC 1939 §7).
    User(String),
    /// `PASS <password>` — specifies the user password (RFC 1939 §7).
    Pass(String),
    /// `STAT` — returns message count and total octets (RFC 1939 §5).
    Stat,
    /// `LIST [msg_num]` — returns size of one or all messages (RFC 1939 §5).
    List(Option<usize>),
    /// `RETR <msg_num>` — retrieves full message content (RFC 1939 §5).
    Retr(usize),
    /// `DELE <msg_num>` — marks message for deletion (RFC 1939 §5).
    Dele(usize),
    /// `NOOP` — does nothing; keeps session alive (RFC 1939 §5).
    Noop,
    /// `RSET` — unmarks all messages marked for deletion (RFC 1939 §5).
    Rset,
    /// `QUIT` — enters UPDATE state, commits deletions, closes session (RFC 1939 §6).
    Quit,
    /// `TOP <msg_num> <lines>` — returns headers plus N body lines (RFC 1939 §7).
    Top(usize, usize),
    /// `UIDL [msg_num]` — returns unique-id for one or all messages (RFC 1939 §7).
    Uidl(Option<usize>),
    /// `CAPA` — returns capability list (RFC 2449).
    Capa,
    /// `STLS` — in-band TLS upgrade on cleartext connection (RFC 2595).
    Stls,
    /// `AUTH <mechanism> [initial-response]` — SASL authentication (RFC 5034).
    Auth {
        /// SASL mechanism, e.g. PLAIN or LOGIN.
        mechanism: String,
        /// Optional initial response.
        initial_response: Option<String>,
    },
    /// An unrecognized or malformed command.
    Unknown(String),
}

/// Why a POP3 command failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The input line was empty.
    #[error("empty command")]
    Empty,
    /// The command was missing a required argument.
    #[error("missing required argument for command: {0}")]
    MissingArgument(&'static str),
    /// An argument could not be parsed as a positive integer.
    #[error("invalid integer argument for {0}: {1}")]
    InvalidInteger(&'static str, String),
}

impl Pop3Command {
    /// Parses a single raw command line.
    ///
    /// # Errors
    /// Returns [`ParseError`] if required arguments are missing or invalid.
    // One arm per POP3 verb: splitting the dispatch would scatter the grammar
    // across helpers without making any single verb easier to read.
    #[allow(clippy::too_many_lines)]
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let mut parts = trimmed.splitn(2, ' ');
        let verb = parts.next().unwrap_or("").trim();
        if verb.is_empty() {
            return Err(ParseError::Empty);
        }

        let rest = parts.next().map(str::trim);

        match verb.to_ascii_uppercase().as_str() {
            "USER" => {
                let user = rest
                    .filter(|s| !s.is_empty())
                    .ok_or(ParseError::MissingArgument("USER"))?;
                Ok(Self::User(user.to_string()))
            }
            "PASS" => {
                // Passwords may be empty or contain spaces; RFC 1939 PASS takes whatever follows.
                let pass = rest.unwrap_or("");
                Ok(Self::Pass(pass.to_string()))
            }
            "STAT" => Ok(Self::Stat),
            "LIST" => {
                let msg_num = match rest {
                    Some(s) if !s.is_empty() => {
                        let num = s
                            .parse::<usize>()
                            .map_err(|_| ParseError::InvalidInteger("LIST", s.to_string()))?;
                        if num == 0 {
                            return Err(ParseError::InvalidInteger("LIST", s.to_string()));
                        }
                        Some(num)
                    }
                    _ => None,
                };
                Ok(Self::List(msg_num))
            }
            "RETR" => {
                let s = rest
                    .filter(|s| !s.is_empty())
                    .ok_or(ParseError::MissingArgument("RETR"))?;
                let num = s
                    .parse::<usize>()
                    .map_err(|_| ParseError::InvalidInteger("RETR", s.to_string()))?;
                if num == 0 {
                    return Err(ParseError::InvalidInteger("RETR", s.to_string()));
                }
                Ok(Self::Retr(num))
            }
            "DELE" => {
                let s = rest
                    .filter(|s| !s.is_empty())
                    .ok_or(ParseError::MissingArgument("DELE"))?;
                let num = s
                    .parse::<usize>()
                    .map_err(|_| ParseError::InvalidInteger("DELE", s.to_string()))?;
                if num == 0 {
                    return Err(ParseError::InvalidInteger("DELE", s.to_string()));
                }
                Ok(Self::Dele(num))
            }
            "NOOP" => Ok(Self::Noop),
            "RSET" => Ok(Self::Rset),
            "QUIT" => Ok(Self::Quit),
            "TOP" => {
                let args = rest
                    .filter(|s| !s.is_empty())
                    .ok_or(ParseError::MissingArgument("TOP"))?;
                let mut arg_parts = args.split_whitespace();
                let msg_str = arg_parts
                    .next()
                    .ok_or(ParseError::MissingArgument("TOP msg_num"))?;
                let lines_str = arg_parts
                    .next()
                    .ok_or(ParseError::MissingArgument("TOP lines"))?;

                let msg_num = msg_str
                    .parse::<usize>()
                    .map_err(|_| ParseError::InvalidInteger("TOP msg_num", msg_str.to_string()))?;
                if msg_num == 0 {
                    return Err(ParseError::InvalidInteger(
                        "TOP msg_num",
                        msg_str.to_string(),
                    ));
                }

                let lines = lines_str
                    .parse::<usize>()
                    .map_err(|_| ParseError::InvalidInteger("TOP lines", lines_str.to_string()))?;

                Ok(Self::Top(msg_num, lines))
            }
            "UIDL" => {
                let msg_num = match rest {
                    Some(s) if !s.is_empty() => {
                        let num = s
                            .parse::<usize>()
                            .map_err(|_| ParseError::InvalidInteger("UIDL", s.to_string()))?;
                        if num == 0 {
                            return Err(ParseError::InvalidInteger("UIDL", s.to_string()));
                        }
                        Some(num)
                    }
                    _ => None,
                };
                Ok(Self::Uidl(msg_num))
            }
            "CAPA" => Ok(Self::Capa),
            "STLS" => Ok(Self::Stls),
            "AUTH" => match rest {
                None | Some("") => Ok(Self::Auth {
                    mechanism: String::new(),
                    initial_response: None,
                }),
                Some(args) => {
                    let mut parts = args.split_whitespace();
                    let mechanism = parts.next().unwrap_or("").to_string();
                    let initial_response = parts.next().map(ToString::to_string);
                    Ok(Self::Auth {
                        mechanism,
                        initial_response,
                    })
                }
            },
            other => Ok(Self::Unknown(other.to_string())),
        }
    }
}

impl fmt::Display for Pop3Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(u) => write!(f, "USER {u}"),
            Self::Pass(_) => write!(f, "PASS ********"),
            Self::Stat => write!(f, "STAT"),
            Self::List(None) => write!(f, "LIST"),
            Self::List(Some(n)) => write!(f, "LIST {n}"),
            Self::Retr(n) => write!(f, "RETR {n}"),
            Self::Dele(n) => write!(f, "DELE {n}"),
            Self::Noop => write!(f, "NOOP"),
            Self::Rset => write!(f, "RSET"),
            Self::Quit => write!(f, "QUIT"),
            Self::Top(n, l) => write!(f, "TOP {n} {l}"),
            Self::Uidl(None) => write!(f, "UIDL"),
            Self::Uidl(Some(n)) => write!(f, "UIDL {n}"),
            Self::Capa => write!(f, "CAPA"),
            Self::Stls => write!(f, "STLS"),
            Self::Auth { mechanism, .. } => write!(f, "AUTH {mechanism}"),
            Self::Unknown(c) => write!(f, "UNKNOWN({c})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_commands() {
        assert_eq!(
            Pop3Command::parse("USER alice\r\n"),
            Ok(Pop3Command::User("alice".into()))
        );
        assert_eq!(
            Pop3Command::parse("user alice\n"),
            Ok(Pop3Command::User("alice".into()))
        );
        assert_eq!(
            Pop3Command::parse("PASS secret123\r\n"),
            Ok(Pop3Command::Pass("secret123".into()))
        );
        assert_eq!(Pop3Command::parse("STAT\r\n"), Ok(Pop3Command::Stat));
        assert_eq!(Pop3Command::parse("LIST\r\n"), Ok(Pop3Command::List(None)));
        assert_eq!(
            Pop3Command::parse("LIST 2\r\n"),
            Ok(Pop3Command::List(Some(2)))
        );
        assert_eq!(Pop3Command::parse("RETR 1\r\n"), Ok(Pop3Command::Retr(1)));
        assert_eq!(Pop3Command::parse("DELE 3\r\n"), Ok(Pop3Command::Dele(3)));
        assert_eq!(Pop3Command::parse("NOOP\r\n"), Ok(Pop3Command::Noop));
        assert_eq!(Pop3Command::parse("RSET\r\n"), Ok(Pop3Command::Rset));
        assert_eq!(Pop3Command::parse("QUIT\r\n"), Ok(Pop3Command::Quit));
        assert_eq!(
            Pop3Command::parse("TOP 1 10\r\n"),
            Ok(Pop3Command::Top(1, 10))
        );
        assert_eq!(Pop3Command::parse("UIDL\r\n"), Ok(Pop3Command::Uidl(None)));
        assert_eq!(
            Pop3Command::parse("UIDL 5\r\n"),
            Ok(Pop3Command::Uidl(Some(5)))
        );
        assert_eq!(Pop3Command::parse("CAPA\r\n"), Ok(Pop3Command::Capa));
        assert_eq!(Pop3Command::parse("STLS\r\n"), Ok(Pop3Command::Stls));
        assert_eq!(
            Pop3Command::parse("AUTH PLAIN\r\n"),
            Ok(Pop3Command::Auth {
                mechanism: "PLAIN".into(),
                initial_response: None,
            })
        );
        assert_eq!(
            Pop3Command::parse("AUTH PLAIN dGVzdA==\r\n"),
            Ok(Pop3Command::Auth {
                mechanism: "PLAIN".into(),
                initial_response: Some("dGVzdA==".into()),
            })
        );
    }

    #[test]
    fn parse_invalid_inputs() {
        assert_eq!(Pop3Command::parse(""), Err(ParseError::Empty));
        assert_eq!(Pop3Command::parse("\r\n"), Err(ParseError::Empty));
        assert_eq!(
            Pop3Command::parse("USER\r\n"),
            Err(ParseError::MissingArgument("USER"))
        );
        assert_eq!(
            Pop3Command::parse("RETR\r\n"),
            Err(ParseError::MissingArgument("RETR"))
        );
        assert_eq!(
            Pop3Command::parse("RETR 0\r\n"),
            Err(ParseError::InvalidInteger("RETR", "0".into()))
        );
        assert_eq!(
            Pop3Command::parse("RETR abc\r\n"),
            Err(ParseError::InvalidInteger("RETR", "abc".into()))
        );
        assert_eq!(
            Pop3Command::parse("TOP 1\r\n"),
            Err(ParseError::MissingArgument("TOP lines"))
        );
        assert_eq!(
            Pop3Command::parse("FOOBAR\r\n"),
            Ok(Pop3Command::Unknown("FOOBAR".into()))
        );
    }
}
