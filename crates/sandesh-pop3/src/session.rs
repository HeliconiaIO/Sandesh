//! POP3 session handler (RFC 1939, RFC 2449, RFC 2595, RFC 5034).

use std::sync::Arc;

use sandesh_config::Pop3Config;
use sandesh_core::{
    Account, AccountStore, EntryId, MailboxName, MailboxStore, MessageId, MessageStore, Uid,
};
use sandesh_proto::{
    Connection, Credentials, Mechanism, ProtocolError, SaslError, Secret, decode_login_field,
    decode_plain, login_prompts, verify_optional,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::command::{ParseError, Pop3Command};
use crate::response::{self, dot_stuff, format_top};
use crate::state::Pop3State;
use std::fmt::Write as _;

/// One message in the session's INBOX snapshot (RFC 1939 §5).
#[derive(Debug, Clone)]
pub struct Pop3Message {
    /// 1-indexed message number, stable for the session's life.
    pub msg_num: usize,
    /// Storage entry id.
    pub entry_id: EntryId,
    /// Domain message id.
    pub message_id: MessageId,
    /// UID within INBOX.
    pub uid: Uid,
    /// Message size in octets (`RFC822.SIZE`).
    pub size: u64,
    /// Whether marked for deletion in this session.
    pub deleted: bool,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The connection closed cleanly or dropped.
    Closed,
    /// The client issued `STLS` and was invited to negotiate TLS.
    UpgradeToTls,
}

/// Whether the command loop should continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop(Outcome),
}

/// A running POP3 session over a framed connection.
pub struct Session<R, W> {
    connection: Connection<R, W>,
    config: Arc<Pop3Config>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages_store: Arc<dyn MessageStore>,
    state: Pop3State,
    encrypted: bool,
    account: Option<Account>,
    messages: Vec<Pop3Message>,
    peer: Option<String>,
}

impl<R, W> std::fmt::Debug for Session<R, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("encrypted", &self.encrypted)
            .field("account", &self.account.as_ref().map(|a| &a.username))
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl<R, W> Session<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a new POP3 session.
    pub fn new(
        connection: Connection<R, W>,
        config: Arc<Pop3Config>,
        accounts: Arc<dyn AccountStore>,
        mailboxes: Arc<dyn MailboxStore>,
        messages_store: Arc<dyn MessageStore>,
        encrypted: bool,
    ) -> Self {
        let peer = connection.peer().map(|addr| addr.to_string());
        Self {
            connection,
            config,
            accounts,
            mailboxes,
            messages_store,
            state: Pop3State::default(),
            encrypted,
            account: None,
            messages: Vec::new(),
            peer,
        }
    }

    /// Consumes the session, returning its underlying connection.
    pub fn into_connection(self) -> Connection<R, W> {
        self.connection
    }

    /// Runs the POP3 session loop.
    ///
    /// If `greet` is true, sends the initial `+OK` greeting line before
    /// reading commands (omitted after STLS upgrade per RFC 2595).
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] if the underlying connection fails while
    /// reading a command or writing a response.
    pub async fn run(mut self, greet: bool) -> Result<Outcome, ProtocolError> {
        if greet {
            let greeting = format!(
                "+OK Sandesh POP3 server ready <{}>\r\n",
                self.config.hostname
            );
            self.connection.write_flush(greeting.as_bytes()).await?;
        }

        loop {
            let line = match self.connection.read_line(self.config.command_timeout).await {
                Ok(line) => line,
                Err(ProtocolError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    // Client disconnected without QUIT: pending deletes are discarded.
                    tracing::debug!(peer = ?self.peer, "POP3 client disconnected; aborting session");
                    return Ok(Outcome::Closed);
                }
                Err(error) => return Err(error),
            };

            let line_str = String::from_utf8_lossy(line).into_owned();
            let command = match Pop3Command::parse(&line_str) {
                Ok(cmd) => cmd,
                Err(ParseError::Empty) => continue,
                Err(err) => {
                    let reply = response::err(&err.to_string());
                    self.connection.write_flush(reply.as_bytes()).await?;
                    continue;
                }
            };

            match self.dispatch(command).await? {
                Flow::Continue => {}
                Flow::Stop(outcome) => return Ok(outcome),
            }
        }
    }

    /// Dispatches one parsed command.
    async fn dispatch(&mut self, command: Pop3Command) -> Result<Flow, ProtocolError> {
        if !self.state.is_command_permitted(&command) {
            let reply = response::err("command not permitted in current state");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        match command {
            Pop3Command::User(username) => self.handle_user(username).await,
            Pop3Command::Pass(password) => self.handle_pass(password).await,
            Pop3Command::Stat => self.handle_stat().await,
            Pop3Command::List(msg_num) => self.handle_list(msg_num).await,
            Pop3Command::Retr(msg_num) => self.handle_retr(msg_num).await,
            Pop3Command::Dele(msg_num) => self.handle_dele(msg_num).await,
            Pop3Command::Noop => self.handle_noop().await,
            Pop3Command::Rset => self.handle_rset().await,
            Pop3Command::Quit => self.handle_quit().await,
            Pop3Command::Top(msg_num, lines) => self.handle_top(msg_num, lines).await,
            Pop3Command::Uidl(msg_num) => self.handle_uidl(msg_num).await,
            Pop3Command::Capa => self.handle_capa().await,
            Pop3Command::Stls => self.handle_stls().await,
            Pop3Command::Auth {
                mechanism,
                initial_response,
            } => self.handle_auth(&mechanism, initial_response).await,
            Pop3Command::Unknown(cmd) => {
                let reply = response::err(&format!("unrecognized command: {cmd}"));
                self.connection.write_flush(reply.as_bytes()).await?;
                Ok(Flow::Continue)
            }
        }
    }

    async fn handle_user(&mut self, username: String) -> Result<Flow, ProtocolError> {
        self.state = Pop3State::Authorization {
            user: Some(username),
        };
        let reply = response::ok("user accepted, send password");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    async fn handle_pass(&mut self, password: String) -> Result<Flow, ProtocolError> {
        let username = if let Pop3State::Authorization { user: Some(u) } = &self.state {
            u.clone()
        } else {
            let reply = response::err("USER command required before PASS");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let stored = self.credential_hash(&username).await;
        if self.config.auth_mode != sandesh_config::AuthMode::Disabled || stored.is_some() {
            let cred_secret = Secret::new(password);
            if !verify_optional(&cred_secret, stored.as_deref()) {
                self.state = Pop3State::Authorization { user: None };
                let reply = response::err("authentication failed");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        }

        match self.enter_transaction(&username).await {
            Ok(summary) => {
                let reply = response::ok(&summary);
                self.connection.write_flush(reply.as_bytes()).await?;
            }
            Err(err_msg) => {
                self.state = Pop3State::Authorization { user: None };
                let reply = response::err(&err_msg);
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        }

        Ok(Flow::Continue)
    }

    /// Acquires the INBOX snapshot and transitions to TRANSACTION state.
    async fn enter_transaction(&mut self, username: &str) -> Result<String, String> {
        let account = match self.accounts.get_by_username(username).await {
            Ok(Some(acc)) => acc,
            Ok(None) => match self.accounts.catch_all().await {
                Ok(Some(acc)) => acc,
                Ok(None) => match self.accounts.list().await {
                    Ok(mut list) if !list.is_empty() => list.remove(0),
                    _ => return Err(String::from("no mailbox found for user")),
                },
                Err(err) => return Err(err.to_string()),
            },
            Err(err) => return Err(err.to_string()),
        };

        let inbox_name = MailboxName::new(sandesh_core::INBOX).map_err(|e| e.to_string())?;
        let inbox = match self.mailboxes.get_by_name(account.id, &inbox_name).await {
            Ok(Some(mbx)) => mbx,
            Ok(None) => return Err(String::from("INBOX not found")),
            Err(err) => return Err(err.to_string()),
        };

        let entries = match self.messages_store.list_entries(inbox.id).await {
            Ok(e) => e,
            Err(err) => return Err(err.to_string()),
        };

        let mut snapshot = Vec::with_capacity(entries.len());
        let mut total_octets: u64 = 0;

        for (index, entry) in entries.into_iter().enumerate() {
            let raw = self.messages_store.raw(entry.message_id).await;
            let size = match raw {
                Ok(Some(r)) => r.size(),
                _ => 0,
            };
            total_octets += size;
            snapshot.push(Pop3Message {
                msg_num: index + 1,
                entry_id: entry.id,
                message_id: entry.message_id,
                uid: entry.uid,
                size,
                deleted: false,
            });
        }

        let count = snapshot.len();
        self.account = Some(account);
        self.messages = snapshot;
        self.state = Pop3State::Transaction;

        Ok(format!("{count} messages ({total_octets} octets)"))
    }

    async fn handle_stat(&mut self) -> Result<Flow, ProtocolError> {
        let active = self.messages.iter().filter(|m| !m.deleted);
        let count = active.clone().count();
        let octets: u64 = active.map(|m| m.size).sum();

        let reply = response::ok(&format!("{count} {octets}"));
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    async fn handle_list(&mut self, msg_num: Option<usize>) -> Result<Flow, ProtocolError> {
        if let Some(n) = msg_num {
            let found = self.messages.iter().find(|m| m.msg_num == n && !m.deleted);
            if let Some(m) = found {
                let reply = response::ok(&format!("{n} {}", m.size));
                self.connection.write_flush(reply.as_bytes()).await?;
            } else {
                let reply = response::err("no such message");
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        } else {
            let active = self.messages.iter().filter(|m| !m.deleted);
            let count = active.clone().count();
            let octets: u64 = active.clone().map(|m| m.size).sum();

            let mut out = format!("+OK {count} messages ({octets} octets)\r\n");
            for m in active {
                let _ = writeln!(out, "{} {}\r", m.msg_num, m.size);
            }
            out.push_str(".\r\n");
            self.connection.write_flush(out.as_bytes()).await?;
        }
        Ok(Flow::Continue)
    }

    async fn handle_uidl(&mut self, msg_num: Option<usize>) -> Result<Flow, ProtocolError> {
        if let Some(n) = msg_num {
            let found = self.messages.iter().find(|m| m.msg_num == n && !m.deleted);
            if let Some(m) = found {
                let reply = response::ok(&format!("{n} {:08x}", m.uid.get()));
                self.connection.write_flush(reply.as_bytes()).await?;
            } else {
                let reply = response::err("no such message");
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        } else {
            let mut out = String::from("+OK unique-id listing follows\r\n");
            for m in self.messages.iter().filter(|m| !m.deleted) {
                let _ = writeln!(out, "{} {:08x}\r", m.msg_num, m.uid.get());
            }
            out.push_str(".\r\n");
            self.connection.write_flush(out.as_bytes()).await?;
        }
        Ok(Flow::Continue)
    }

    async fn handle_retr(&mut self, msg_num: usize) -> Result<Flow, ProtocolError> {
        let found = self
            .messages
            .iter()
            .find(|m| m.msg_num == msg_num && !m.deleted);

        let msg = if let Some(m) = found {
            m.clone()
        } else {
            let reply = response::err("no such message");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let Ok(Some(raw)) = self.messages_store.raw(msg.message_id).await else {
            let reply = response::err("message content unavailable");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        // No octet count. RFC 1939 §5 makes it optional ("+OK message follows"
        // is a conforming greeting) and the client reads to the '.' terminator
        // regardless. The only count worth printing would be the post-stuffing
        // wire length, which is neither the stored `RFC822.SIZE` that `LIST`
        // and `STAT` report nor a figure any client uses — so printing it would
        // invite the reader to believe the three numbers agree when they do not.
        let header_line = "+OK message follows\r\n";
        let stuffed_body = dot_stuff(raw.bytes());

        self.connection.write(header_line.as_bytes()).await?;
        self.connection.write_flush(&stuffed_body).await?;
        Ok(Flow::Continue)
    }

    async fn handle_top(&mut self, msg_num: usize, lines: usize) -> Result<Flow, ProtocolError> {
        let found = self
            .messages
            .iter()
            .find(|m| m.msg_num == msg_num && !m.deleted);

        let msg = if let Some(m) = found {
            m.clone()
        } else {
            let reply = response::err("no such message");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let Ok(Some(raw)) = self.messages_store.raw(msg.message_id).await else {
            let reply = response::err("message content unavailable");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        let header_line = b"+OK\r\n";
        let top_content = format_top(raw.bytes(), lines);

        self.connection.write(header_line).await?;
        self.connection.write_flush(&top_content).await?;
        Ok(Flow::Continue)
    }

    async fn handle_dele(&mut self, msg_num: usize) -> Result<Flow, ProtocolError> {
        let pos = self.messages.iter().position(|m| m.msg_num == msg_num);

        if let Some(idx) = pos {
            if self.messages[idx].deleted {
                let reply = response::err(&format!("message {msg_num} already deleted"));
                self.connection.write_flush(reply.as_bytes()).await?;
            } else {
                self.messages[idx].deleted = true;
                let reply = response::ok(&format!("message {msg_num} marked for deletion"));
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        } else {
            let reply = response::err("no such message");
            self.connection.write_flush(reply.as_bytes()).await?;
        }
        Ok(Flow::Continue)
    }

    async fn handle_rset(&mut self) -> Result<Flow, ProtocolError> {
        for m in &mut self.messages {
            m.deleted = false;
        }
        let active = self.messages.iter();
        let count = active.clone().count();
        let octets: u64 = active.map(|m| m.size).sum();

        let reply = response::ok(&format!("{count} messages ({octets} octets)"));
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    async fn handle_noop(&mut self) -> Result<Flow, ProtocolError> {
        let reply = response::ok("");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    async fn handle_capa(&mut self) -> Result<Flow, ProtocolError> {
        let mut out = String::from(
            "+OK Capability list follows\r\n\
             USER\r\n\
             RESP-CODES\r\n\
             PIPELINING\r\n\
             UIDL\r\n\
             TOP\r\n\
             IMPLEMENTATION Sandesh 0.1.0\r\n",
        );

        if self.config.start_tls && !self.encrypted {
            out.push_str("STLS\r\n");
        }

        if self.config.auth_mode.is_offered_over(self.encrypted) {
            out.push_str("SASL PLAIN LOGIN\r\n");
        }

        out.push_str(".\r\n");
        self.connection.write_flush(out.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    async fn handle_stls(&mut self) -> Result<Flow, ProtocolError> {
        if !self.config.start_tls {
            let reply = response::err("STLS not supported");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        if self.encrypted {
            let reply = response::err("connection already encrypted");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let reply = response::ok("Begin TLS negotiation");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Stop(Outcome::UpgradeToTls))
    }

    async fn credential_hash(&self, username: &str) -> Option<String> {
        match self.accounts.get_by_username(username).await {
            Ok(Some(account)) => account.credential.map(|credential| credential.hash),
            Ok(None) => None,
            Err(error) => {
                tracing::error!(%error, "cannot read account for POP3 authentication");
                None
            }
        }
    }

    async fn handle_auth(
        &mut self,
        mechanism_name: &str,
        initial: Option<String>,
    ) -> Result<Flow, ProtocolError> {
        let Some(mechanism) = Mechanism::parse(mechanism_name) else {
            let reply = response::err("unsupported authentication mechanism");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        };

        if !self.config.auth_mode.is_offered_over(self.encrypted)
            && self.config.auth_mode != sandesh_config::AuthMode::Disabled
        {
            let reply = response::err("authentication not permitted on cleartext connection");
            self.connection.write_flush(reply.as_bytes()).await?;
            return Ok(Flow::Continue);
        }

        let credentials = match mechanism {
            Mechanism::Plain => self.collect_plain(initial.map(Secret::new)).await?,
            Mechanism::Login => self.collect_login(initial.map(Secret::new)).await?,
        };

        let creds = match credentials {
            Ok(c) => c,
            Err(SaslError::Cancelled) => {
                let reply = response::err("authentication cancelled");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
            Err(_) => {
                let reply = response::err("authentication failed");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        };

        if self.config.auth_mode != sandesh_config::AuthMode::Disabled {
            let stored = self.credential_hash(&creds.username).await;
            if !verify_optional(&creds.password, stored.as_deref()) {
                let reply = response::err("authentication failed");
                self.connection.write_flush(reply.as_bytes()).await?;
                return Ok(Flow::Continue);
            }
        }

        match self.enter_transaction(&creds.username).await {
            Ok(summary) => {
                let reply = response::ok(&format!("authenticated, {summary}"));
                self.connection.write_flush(reply.as_bytes()).await?;
            }
            Err(err_msg) => {
                let reply = response::err(&err_msg);
                self.connection.write_flush(reply.as_bytes()).await?;
            }
        }

        Ok(Flow::Continue)
    }

    async fn collect_plain(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, SaslError>, ProtocolError> {
        let encoded = if let Some(i) = initial {
            i
        } else {
            let challenge = b"+ \r\n";
            self.connection.write_flush(challenge).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(err) => return Ok(Err(err)),
            }
        };

        Ok(decode_plain(encoded.expose()))
    }

    async fn collect_login(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, SaslError>, ProtocolError> {
        let [username_prompt, password_prompt] = login_prompts();

        let username_encoded = if let Some(i) = initial {
            i
        } else {
            let challenge = format!("+ {username_prompt}\r\n");
            self.connection.write_flush(challenge.as_bytes()).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(err) => return Ok(Err(err)),
            }
        };

        let username = match decode_login_field(username_encoded.expose()) {
            Ok(u) => u,
            Err(err) => return Ok(Err(err)),
        };

        let challenge = format!("+ {password_prompt}\r\n");
        self.connection.write_flush(challenge.as_bytes()).await?;
        let password_encoded = match self.read_sasl_line().await? {
            Ok(line) => line,
            Err(err) => return Ok(Err(err)),
        };

        let password = match decode_login_field(password_encoded.expose()) {
            Ok(p) => p,
            Err(err) => return Ok(Err(err)),
        };

        Ok(Ok(Credentials {
            username,
            password: Secret::new(password),
        }))
    }

    async fn read_sasl_line(&mut self) -> Result<Result<Secret, SaslError>, ProtocolError> {
        let line = self
            .connection
            .read_line_owned(self.config.command_timeout)
            .await?;

        if line.as_slice() == b"*" {
            return Ok(Err(SaslError::Cancelled));
        }

        Ok(Ok(Secret::new(String::from_utf8_lossy(&line).into_owned())))
    }

    async fn handle_quit(&mut self) -> Result<Flow, ProtocolError> {
        if matches!(self.state, Pop3State::Transaction) {
            self.state = Pop3State::Update;
            for m in &self.messages {
                if m.deleted
                    && let Err(err) = self.messages_store.delete_entry(m.entry_id).await
                {
                    tracing::error!(entry_id = %m.entry_id, %err, "failed to delete entry on POP3 QUIT");
                }
            }
        }

        let reply = response::ok("Sandesh POP3 server signing off");
        self.connection.write_flush(reply.as_bytes()).await?;
        Ok(Flow::Stop(Outcome::Closed))
    }
}
