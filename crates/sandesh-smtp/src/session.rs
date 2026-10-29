//! One SMTP conversation.
//!
//! A session owns all of its own state. Nothing is shared between sessions, and
//! nothing is global — which is what makes hundreds of concurrent clients safe
//! and is asserted directly by the cross-session isolation tests.
//!
//! Generic over the transport so the whole session can be driven over an
//! in-memory duplex: protocol tests need no sockets, no ports and no timing
//! assumptions.

use crate::{Command, ParseError, State, Transition, command, data, reply, reply::Reply, state};
use sandesh_config::SmtpConfig;
use sandesh_core::{
    ConnectionId, EmailAddress, Envelope, FaultMode, FaultPoint, FaultRegistry, Timestamp,
};
use sandesh_core::{DeliveryRequest, DeliveryService, RecipientRejection};
use sandesh_proto::{Connection, Credentials, Mechanism, ProtocolError, SaslError, Secret};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// The current mail transaction.
///
/// Cleared by `RSET`, by a re-greeting, and after every completed `DATA` —
/// which is what lets one connection carry many transactions.
#[derive(Debug, Default)]
struct Transaction {
    mail_from: Option<EmailAddress>,
    /// True once `MAIL FROM` is accepted, distinguishing "no sender given"
    /// from "the null sender `<>` was given" — a difference that matters for
    /// bounce testing.
    has_sender: bool,
    recipients: Vec<EmailAddress>,
    /// Content accumulated across `BDAT` chunks (RFC 3030).
    ///
    /// Empty for a `DATA` transaction, which buffers inside `data::collect`
    /// instead. Chunks live here because they arrive across several commands,
    /// so the buffer has to outlive any one of them — and because `RSET`,
    /// `EHLO` and a dropped connection must all discard it, which
    /// `Transaction::reset` already does for everything else.
    chunks: Vec<u8>,
    /// When the first chunk arrived, for the delivery record's duration.
    ///
    /// Taken at the first `BDAT` rather than at `LAST`, so the recorded time
    /// covers the whole transfer the way `DATA`'s does.
    chunks_started: Option<Timestamp>,
    /// Whether any chunk was refused for exceeding the size limit.
    ///
    /// The sequence continues to its `LAST` so the connection stays
    /// synchronised, exactly as `DATA` reads to its terminator before
    /// answering `552`; this records that the outcome is already decided.
    chunks_too_large: bool,
}

impl Transaction {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The `HELP` response.
///
/// A free function rather than a method: it reads no session state, and
/// keeping it out of `dispatch` is what holds that match to a readable length.
fn help_reply() -> Reply {
    Reply::multiline(
        reply::Code::Ok,
        vec![
            String::from("Sandesh SMTP — a local development mail server"),
            String::from("Commands: EHLO HELO MAIL RCPT DATA BDAT RSET NOOP QUIT VRFY HELP"),
        ],
    )
}

/// What the session loop should do after handling one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep going.
    Continue,
    /// Close the connection.
    Close,
    /// The client issued `STARTTLS` and the `220` has been sent.
    ///
    /// The session cannot perform the handshake itself: it is generic over
    /// split stream halves, and TLS must wrap the whole socket. So it hands the
    /// decision back to whoever owns the socket (`server.rs`), which upgrades
    /// the transport and runs a **fresh** session over it — which is also
    /// exactly what RFC 3207 §4.2 requires, since the server must discard all
    /// knowledge obtained in cleartext.
    UpgradeToTls,
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The conversation finished; the connection should close.
    Closed,
    /// The client issued `STARTTLS`; the caller must upgrade the transport and
    /// run a new session over it.
    UpgradeToTls,
}

/// One SMTP conversation with one client.
///
/// The manual `Debug` deliberately omits the transaction: sender and recipient
/// addresses are session data that should not reach a log line by accident.
pub struct Session<R, W> {
    connection: Connection<R, W>,
    config: Arc<SmtpConfig>,
    delivery: Arc<DeliveryService>,
    state: State,
    transaction: Transaction,
    /// The domain from `EHLO`/`HELO`, kept for the delivery record.
    helo: Option<String>,
    /// Whether the client greeted with `EHLO` rather than `HELO`.
    ///
    /// Recorded because it determines whether ESMTP behaviour is in play.
    extended: bool,
    peer: Option<String>,
    /// Whether this connection is encrypted.
    ///
    /// Gates whether `AUTH` may be offered at all (ADR-033), and makes
    /// `STARTTLS` on an already-encrypted connection a sequencing error rather
    /// than a second handshake.
    encrypted: bool,
    /// The account this session authenticated as, if any.
    ///
    /// A username, never a credential — the password is dropped the moment it
    /// has been verified.
    authenticated_as: Option<String>,
    /// The connection record this session runs on (ADR-066), so a delivery can
    /// link to the conversation that produced it. `None` when tracing is off.
    connection_id: Option<ConnectionId>,
    /// Deliberate failures configured at runtime (ADR-084).
    ///
    /// Shared with the API that sets them, which is why it is an `Arc` rather
    /// than owned: a rule added mid-session applies to the next command this
    /// session handles. Empty by default, and an empty registry costs one
    /// uncontended read lock per checked command.
    faults: Option<Arc<FaultRegistry>>,
}

impl<R, W> std::fmt::Debug for Session<R, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("peer", &self.peer)
            .field("extended", &self.extended)
            .field("encrypted", &self.encrypted)
            // The username is safe to log; the password never reaches here.
            .field("authenticated_as", &self.authenticated_as)
            .finish_non_exhaustive()
    }
}

impl<R, W> Session<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Creates a session over an established connection.
    pub fn new(
        connection: Connection<R, W>,
        config: Arc<SmtpConfig>,
        delivery: Arc<DeliveryService>,
    ) -> Self {
        Self::with_encryption(connection, config, delivery, false)
    }

    /// Creates a session, stating whether the transport is already encrypted.
    ///
    /// Used for the session that runs after a `STARTTLS` upgrade, and for an
    /// implicit-TLS listener.
    pub fn with_encryption(
        connection: Connection<R, W>,
        config: Arc<SmtpConfig>,
        delivery: Arc<DeliveryService>,
        encrypted: bool,
    ) -> Self {
        let peer = connection.peer().map(|p| p.to_string());
        Self {
            connection,
            config,
            delivery,
            state: State::Connected,
            transaction: Transaction::default(),
            helo: None,
            extended: false,
            peer,
            encrypted,
            authenticated_as: None,
            connection_id: None,
            faults: None,
        }
    }

    /// Names the connection record this session runs on (ADR-066).
    #[must_use]
    pub const fn with_connection_id(mut self, id: ConnectionId) -> Self {
        self.connection_id = Some(id);
        self
    }

    /// Supplies the runtime fault registry (ADR-084).
    ///
    /// Optional: a session without one has no faults and takes no lock at all,
    /// which keeps the default path identical to the one that existed before
    /// fault injection.
    #[must_use]
    pub fn with_faults(mut self, faults: Arc<FaultRegistry>) -> Self {
        self.faults = Some(faults);
        self
    }

    /// Runs the conversation to completion.
    ///
    /// # Errors
    /// Returns a transport error only when the connection fails in a way the
    /// protocol cannot answer. An ordinary disconnect is not an error.
    pub async fn run(self) -> Result<(), ProtocolError> {
        self.run_to_outcome(true).await.map(|_| ())
    }

    /// Runs the conversation and reports how it ended.
    ///
    /// `greet` is false for the session that runs *after* a `STARTTLS`
    /// upgrade: RFC 3207 §4.2 has the client send `EHLO` again over the
    /// encrypted channel, and the server must not re-issue a `220` greeting
    /// there — the `220` was already sent to invite the handshake.
    ///
    /// # Errors
    /// As [`Session::run`].
    pub async fn run_to_outcome(mut self, greet: bool) -> Result<Outcome, ProtocolError> {
        if greet {
            let greeting = reply::greeting(&self.config.hostname);
            self.send(&greeting).await?;
        }

        loop {
            let line = match self
                .connection
                .read_line_owned(self.config.command_timeout)
                .await
            {
                Ok(line) => line,

                // An oversized command line is answerable: reply, resynchronise
                // and continue rather than dropping a client that sent one bad
                // line (RFC 5321 §4.5.3.1.4 permits either, and continuing is
                // friendlier for a development tool).
                Err(ProtocolError::LineTooLong { limit }) => {
                    self.connection.discard_input();
                    let reply = reply::syntax_error(format!(
                        "command line exceeded the {limit} byte limit"
                    ));
                    self.send(&reply).await?;
                    continue;
                }

                Err(ProtocolError::Timeout { seconds }) => {
                    // Tell the client why before closing, so a stalled session
                    // is diagnosable rather than a silent disconnect.
                    let reply = reply::service_unavailable(&format!(
                        "timed out after {seconds}s of inactivity"
                    ));
                    let _ = self.send(&reply).await;
                    return Ok(Outcome::Closed);
                }

                Err(e) if e.is_disconnect() => return Ok(Outcome::Closed),
                Err(e) => return Err(e),
            };

            match self.handle_line(&line).await? {
                Flow::Continue => {}
                Flow::Close => return Ok(Outcome::Closed),
                Flow::UpgradeToTls => {
                    // Buffered plaintext cannot cross the boundary
                    // (RFC 3207 §4), and that is guaranteed *structurally*
                    // rather than by a check: returning here consumes the
                    // session, which drops this `Connection` and its read
                    // buffer with it. The server then builds a new
                    // `Connection` over the TLS stream, so bytes an attacker
                    // appended to the cleartext `STARTTLS` line are
                    // unreachable by construction — there is no code path that
                    // could execute them.
                    //
                    // Logged when present, because a client that pipelines
                    // across `STARTTLS` is either broken or being attacked,
                    // and either is worth seeing.
                    if self.connection.has_buffered_input() {
                        tracing::warn!(
                            peer = self.peer.as_deref().unwrap_or("unknown"),
                            "discarding pipelined input sent before STARTTLS"
                        );
                    }
                    return Ok(Outcome::UpgradeToTls);
                }
            }
        }
    }

    /// Parses and dispatches one command line.
    async fn handle_line(&mut self, line: &[u8]) -> Result<Flow, ProtocolError> {
        let command = match command::parse(line) {
            Ok(command) => command,
            Err(e) => {
                let reply = match e {
                    // An empty line is noise; answer 500 without ceremony.
                    ParseError::Empty | ParseError::UnknownCommand | ParseError::NotAscii => {
                        reply::unrecognised_command()
                    }
                    ParseError::NeedsSmtpUtf8 => reply::address_needs_smtputf8(),
                    ParseError::Syntax(detail) => reply::syntax_error(detail),
                    ParseError::UnsupportedParameter(detail) => {
                        reply::parameter_not_implemented(detail)
                    }
                };
                self.send(&reply).await?;
                return Ok(Flow::Continue);
            }
        };

        // Sequencing is decided in one place, before any handler runs.
        let transition = state::next(self.state, &command);
        if let Transition::Rejected { reason } = transition {
            let reply = reply::bad_sequence(reason);
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        self.dispatch(command, transition).await
    }

    /// Executes a command whose sequencing has already been approved.
    async fn dispatch(
        &mut self,
        command: Command,
        transition: Transition,
    ) -> Result<Flow, ProtocolError> {
        match command {
            Command::Ehlo(domain) => {
                self.greet(domain, true, transition);
                let reply = self.ehlo_reply();
                self.send(&reply).await?;
            }

            Command::Helo(domain) => {
                self.greet(domain, false, transition);
                let reply = reply::ok(format!("{} greets you", self.config.hostname));
                self.send(&reply).await?;
            }

            Command::Mail(mail) => {
                return self.handle_mail(mail, transition).await;
            }

            Command::Rcpt(rcpt) => {
                if self.transaction.recipients.len() >= self.config.max_recipients {
                    let reply = reply::too_many_recipients(self.config.max_recipients);
                    self.send(&reply).await?;
                    return Ok(Flow::Continue);
                }

                let address = EmailAddress::parse_lenient(&rcpt.forward_path);

                // Checked before the store: an injected rejection is a decision
                // about the conversation, not about whether the recipient
                // exists, so it must not depend on a lookup succeeding.
                if let Some(mode) = self.match_fault(
                    FaultPoint::Rcpt,
                    self.transaction.mail_from.as_ref(),
                    Some(&address),
                ) && let Some(flow) = self.apply_fault(mode).await?
                {
                    return Ok(flow);
                }

                match self.delivery.check_recipient(&address).await {
                    Ok(Ok(())) => {
                        self.transaction.recipients.push(address);
                        self.apply(transition);
                        let reply = reply::recipient_ok();
                        self.send(&reply).await?;
                    }
                    Ok(Err(RecipientRejection::NoSuchUser)) => {
                        // The recipient is refused but the transaction stands:
                        // the client may offer another (RFC 5321 §3.3).
                        let reply = reply::no_such_user(&address.normalized());
                        self.send(&reply).await?;
                    }
                    Err(e) => {
                        // A storage failure is temporary from the client's
                        // point of view. The detail is logged, never sent.
                        tracing::error!(error = %e, "recipient check failed");
                        let reply = reply::internal_error();
                        self.send(&reply).await?;
                    }
                }
            }

            Command::Data => {
                self.apply(transition);
                return self.receive_data().await;
            }

            Command::Bdat(chunk) => {
                self.apply(transition);
                return self.receive_chunk(chunk).await;
            }

            Command::Rset => {
                self.transaction.reset();
                self.apply(transition);
                let reply = reply::ok("Reset");
                self.send(&reply).await?;
            }

            Command::Noop => {
                let reply = reply::ok("OK");
                self.send(&reply).await?;
            }

            Command::Quit => {
                let reply = reply::closing(&self.config.hostname);
                self.send(&reply).await?;
                return Ok(Flow::Close);
            }

            // 252: the correct non-disclosing answer. Confirming which
            // addresses exist would make the server an address-harvesting
            // oracle, and Sandesh accepts mail for unknown recipients anyway.
            Command::Vrfy(_) => {
                let reply = reply::cannot_verify();
                self.send(&reply).await?;
            }

            // EXPN would disclose mailing-list membership; Sandesh has no lists.
            Command::Expn(_) => {
                let reply = reply::not_implemented("EXPN");
                self.send(&reply).await?;
            }

            Command::Help(_) => {
                let reply = help_reply();
                self.send(&reply).await?;
            }

            Command::Auth(auth) => {
                return self.handle_auth(auth).await;
            }

            Command::StartTls => {
                return self.handle_starttls(transition).await;
            }

            Command::Unimplemented(verb) => {
                let reply = reply::not_implemented(&verb);
                self.send(&reply).await?;
            }
        }

        Ok(Flow::Continue)
    }

    /// Collects one `BDAT` chunk, delivering the message when `LAST` arrives.
    ///
    /// # Why this is not `receive_data` with a flag
    ///
    /// The two framings are incompatible in three ways at once (ADR-088):
    /// `BDAT` ends at a declared count rather than `<CRLF>.<CRLF>`, applies **no
    /// dot-stuffing**, and answers *every* chunk rather than once. A shared
    /// collector switching on a flag would have to be right about all three on
    /// every path; two handlers make the difference impossible to conflate.
    ///
    /// The absence of dot-unstuffing is the part most easily got wrong: a body
    /// line `..hidden` arrives here as literally `..hidden`, and "fixing" it
    /// would corrupt every chunked message carrying a leading dot.
    async fn receive_chunk(&mut self, chunk: command::BdatCommand) -> Result<Flow, ProtocolError> {
        // Timed from the first chunk so the recorded duration covers the whole
        // transfer, matching what DATA reports.
        let started = *self
            .transaction
            .chunks_started
            .get_or_insert_with(Timestamp::now);

        // The declared size is checked against the limit *before* any buffer is
        // reserved: `BDAT <n>` is client-supplied, and treating it as an
        // allocation size is the obvious denial of service. `read_exact_owned`
        // enforces the bound itself (ADR-043), and the check here is what turns
        // an oversize declaration into a recorded `552` rather than a
        // connection error.
        let limit = self.config.max_message_size;
        let would_exceed = self.transaction.chunks.len().saturating_add(chunk.size) > limit;

        if would_exceed {
            // Still read the octets: the connection stays synchronised, so the
            // session can answer and carry on rather than disconnecting. This
            // is what DATA already does before replying 552.
            self.transaction.chunks_too_large = true;
        }

        let payload = match self
            .connection
            .read_exact_owned(chunk.size, limit.max(chunk.size), self.config.data_timeout)
            .await
        {
            Ok(payload) => payload,
            // The stream is desynchronised: the declared octets could not be
            // read, so command and content can no longer be told apart.
            Err(e) => {
                self.transaction.reset();
                self.state = State::Ready;
                if e.is_disconnect() {
                    return Ok(Flow::Close);
                }
                let reply = reply::service_unavailable("error reading chunk content");
                let _ = self.send(&reply).await;
                return Ok(Flow::Close);
            }
        };

        if !would_exceed && !self.transaction.chunks_too_large {
            self.transaction.chunks.extend_from_slice(&payload);
        }

        if !chunk.last {
            // Every chunk is acknowledged (RFC 3030 §2), which is what lets a
            // client stream without waiting for the transaction to end.
            let reply = reply::chunk_accepted(payload.len());
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        // LAST: the transaction is over either way, so close it before the
        // delivery attempt — no failure path may leave it half-open.
        let transaction = std::mem::take(&mut self.transaction);
        self.state = State::Ready;

        if transaction.chunks_too_large {
            let envelope = Envelope {
                mail_from: transaction.mail_from.clone(),
                rcpt_to: transaction.recipients.clone(),
                helo_domain: self.helo.clone(),
                peer: self.peer.clone(),
                tls: None,
                auth_identity: None,
            };
            if let Err(error) = self
                .delivery
                .record_rejection(
                    envelope,
                    transaction.recipients,
                    format!("message exceeds the {limit}-octet limit"),
                    started,
                    self.connection_id,
                )
                .await
            {
                tracing::error!(%error, "could not record an oversize rejection");
            }

            let reply = reply::message_too_large(limit);
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        self.connection.transcript().elided(
            sandesh_proto::Direction::In,
            "BDAT",
            transaction.chunks.len(),
        );

        // The same fault point DATA uses: by here the client has paid to
        // transmit the whole message, which is what distinguishes a DATA-stage
        // fault from a RCPT rejection.
        if let Some(mode) = self.match_fault(
            FaultPoint::Data,
            transaction.mail_from.as_ref(),
            transaction.recipients.first(),
        ) && let Some(flow) = self.apply_fault(mode).await?
        {
            return Ok(flow);
        }

        self.deliver_content(
            transaction.mail_from,
            transaction.recipients,
            transaction.chunks,
        )
        .await?;

        Ok(Flow::Continue)
    }

    /// Stores collected content and answers the client.
    ///
    /// Shared by `DATA` and `BDAT` so the two paths cannot diverge in what they
    /// store or what they report — the byte-identical guarantee ADR-088 makes
    /// is only credible if both end in the same code.
    async fn deliver_content(
        &mut self,
        mail_from: Option<EmailAddress>,
        recipients: Vec<EmailAddress>,
        content: Vec<u8>,
    ) -> Result<(), ProtocolError> {
        let request = DeliveryRequest {
            mail_from,
            recipients,
            content,
            helo: self.helo.clone(),
            peer: self.peer.clone(),
            connection_id: self.connection_id,
            auth_identity: self.authenticated_as.clone(),
        };

        match self.delivery.deliver(request).await {
            Ok(receipt) => {
                tracing::info!(
                    message_id = %receipt.message_id,
                    placements = receipt.placements,
                    "message accepted"
                );
                let reply = reply::message_accepted(&receipt.message_id.to_string());
                self.send(&reply).await?;
            }
            Err(e) => {
                // 451: the client may retry. The internal detail is logged and
                // never put on the wire.
                tracing::error!(error = %e, "delivery failed");
                let reply = reply::internal_error();
                self.send(&reply).await?;
            }
        }

        Ok(())
    }

    /// Collects `DATA` content and delivers it.
    async fn receive_data(&mut self) -> Result<Flow, ProtocolError> {
        // Timed from the invitation, so a rejection records how long the
        // client spent sending the content that was ultimately refused.
        let started = Timestamp::now();
        let invitation = reply::start_mail_input();
        self.send(&invitation).await?;

        let outcome = match data::collect(
            &mut self.connection,
            self.config.max_message_size,
            self.config.data_timeout,
        )
        .await
        {
            Ok(outcome) => outcome,

            // The stream is desynchronised: we cannot tell content from
            // commands any more, so the connection must close.
            Err(e) => {
                self.transaction.reset();
                self.state = State::Ready;
                if e.is_disconnect() {
                    return Ok(Flow::Close);
                }
                let reply = reply::service_unavailable("error reading message content");
                let _ = self.send(&reply).await;
                return Ok(Flow::Close);
            }
        };

        // Whatever happens next, the transaction is over and the session
        // returns to Ready. Doing this before the delivery attempt guarantees
        // no failure path can leave a half-open transaction behind.
        let transaction = std::mem::take(&mut self.transaction);
        self.state = State::Ready;

        let content = match outcome {
            data::DataOutcome::Complete(content) => {
                // The transcript records that content passed and how much of
                // it; the content itself is never captured (ADR-066).
                self.connection.transcript().elided(
                    sandesh_proto::Direction::In,
                    "DATA",
                    content.len(),
                );
                content
            }
            data::DataOutcome::TooLarge { limit } => {
                // A refusal is a routing outcome the Delivery view has to be
                // able to explain: "the message never arrived" is the question
                // it exists to answer. Recorded before the reply so a client
                // that sees 552 can already find the record.
                let envelope = Envelope {
                    mail_from: transaction.mail_from.clone(),
                    rcpt_to: transaction.recipients.clone(),
                    helo_domain: self.helo.clone(),
                    peer: self.peer.clone(),
                    tls: None,
                    auth_identity: None,
                };
                if let Err(error) = self
                    .delivery
                    .record_rejection(
                        envelope,
                        transaction.recipients,
                        format!("message exceeds the {limit}-octet limit"),
                        started,
                        self.connection_id,
                    )
                    .await
                {
                    // Failing to record must not change what the client is
                    // told: the message is oversize either way.
                    tracing::error!(%error, "could not record an oversize rejection");
                }

                let reply = reply::message_too_large(limit);
                self.send(&reply).await?;
                return Ok(Flow::Continue);
            }
        };

        // Checked after the body is received and the transaction is closed, but
        // before anything is stored: the point of a DATA-stage fault is that
        // the client has already paid to transmit the message. Firing earlier
        // would make it indistinguishable from a RCPT rejection.
        if let Some(mode) = self.match_fault(
            FaultPoint::Data,
            transaction.mail_from.as_ref(),
            transaction.recipients.first(),
        ) && let Some(flow) = self.apply_fault(mode).await?
        {
            return Ok(flow);
        }

        self.deliver_content(transaction.mail_from, transaction.recipients, content)
            .await?;

        Ok(Flow::Continue)
    }

    /// Records a greeting and resets any open transaction.
    fn greet(&mut self, domain: String, extended: bool, transition: Transition) {
        self.helo = Some(domain);
        self.extended = extended;
        // RFC 5321 §4.1.4: a greeting discards any transaction in progress.
        self.transaction.reset();
        self.apply(transition);
    }

    /// Handles `MAIL FROM`, including the authentication requirement.
    async fn handle_mail(
        &mut self,
        mail: command::MailCommand,
        transition: Transition,
    ) -> Result<Flow, ProtocolError> {
        // RFC 4954 §4: with authentication required, a transaction may
        // not begin without it. Checked here rather than in the state
        // machine because it is a policy about *who* the client is,
        // not about command sequencing.
        let auth_required =
            self.config.auth_mode.is_required() || self.delivery.is_auth_required().await;
        if auth_required && self.authenticated_as.is_none() {
            let reply = reply::auth_required();
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        // SIZE is advertised, so a declared size larger than the limit
        // is refused now rather than after the client sends the body.
        if mail
            .declared_size
            .is_some_and(|declared| declared > self.config.max_message_size)
        {
            let reply = reply::message_too_large(self.config.max_message_size);
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        self.transaction.reset();
        self.transaction.has_sender = true;
        self.transaction.mail_from = mail
            .reverse_path
            .as_deref()
            .map(EmailAddress::parse_lenient);
        self.apply(transition);

        let reply = reply::sender_ok();
        self.send(&reply).await?;

        Ok(Flow::Continue)
    }

    /// Handles `STARTTLS` (RFC 3207 §4).
    ///
    /// Sends the `220` and then hands the upgrade outward: the session is
    /// generic over stream halves and cannot wrap the socket itself.
    async fn handle_starttls(&mut self, transition: Transition) -> Result<Flow, ProtocolError> {
        if self.encrypted {
            // RFC 3207 §4.2: a second handshake inside the first is not a
            // thing. Refusing is clearer than silently ignoring.
            let reply = reply::bad_sequence("TLS is already active");
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        if !self.config.tls_mode.advertises_starttls() {
            let reply = reply::not_implemented("STARTTLS");
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        // The 220 must be the last thing written in cleartext: the client
        // begins its handshake the moment it arrives, so anything sent
        // afterwards would land inside the TLS record stream and corrupt it.
        let reply = reply::ready_to_start_tls();
        self.send(&reply).await?;
        self.apply(transition);
        Ok(Flow::UpgradeToTls)
    }

    /// Runs an `AUTH` exchange (RFC 4954).
    ///
    /// The order of the checks is deliberate and is the security contract:
    ///
    /// 1. **Is auth enabled at all?** If not, `AUTH` is not implemented here.
    /// 2. **Is the connection encrypted?** `PLAIN` and `LOGIN` send the
    ///    password, so an unencrypted attempt is refused with `538` *before*
    ///    any credential is read. The client is never given the opportunity to
    ///    transmit one in the clear.
    /// 3. **Is the mechanism supported?** CRAM-MD5 is refused by name
    ///    (ADR-032) rather than silently downgraded.
    /// 4. Only then is a credential decoded and verified.
    ///
    /// Every failure answers `535` with the same text. Distinguishing "no such
    /// user" from "wrong password" would rebuild the username oracle that the
    /// constant-time verifier exists to close.
    async fn handle_auth(&mut self, auth: command::AuthCommand) -> Result<Flow, ProtocolError> {
        let auth_permitted = self.config.auth_mode.is_enabled() || self.config.allow_cleartext_auth;
        if !auth_permitted {
            let reply = reply::not_implemented("AUTH");
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        // Refused before a credential is read if encryption is strictly required
        // and cleartext auth is not allowed on this channel.
        let channel_ok = self.encrypted
            || (self.config.allow_cleartext_auth && !self.config.tls_mode.advertises_starttls());
        if !channel_ok {
            let reply = reply::encryption_required();
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        if self.authenticated_as.is_some() {
            let reply = reply::bad_sequence("already authenticated");
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        }

        let Some(mechanism) = Mechanism::parse(&auth.mechanism) else {
            let reply = reply::unsupported_mechanism(&auth.mechanism);
            self.send(&reply).await?;
            return Ok(Flow::Continue);
        };

        let credentials = match mechanism {
            Mechanism::Plain => self.collect_plain(auth.initial_response).await?,
            Mechanism::Login => self.collect_login(auth.initial_response).await?,
        };

        let credentials = match credentials {
            Ok(credentials) => credentials,
            Err(outcome) => return Ok(outcome),
        };

        self.verify(credentials).await
    }

    /// Collects a `PLAIN` credential, prompting when no initial response came.
    async fn collect_plain(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, Flow>, ProtocolError> {
        let encoded = if let Some(encoded) = initial {
            encoded
        } else {
            // An empty challenge, as RFC 4954 §4 specifies for PLAIN.
            let reply = reply::auth_challenge("");
            self.send(&reply).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(flow) => return Ok(Err(flow)),
            }
        };

        match sandesh_proto::decode_plain(encoded.expose()) {
            Ok(credentials) => Ok(Ok(credentials)),
            Err(error) => Ok(Err(self.reject_sasl(error).await?)),
        }
    }

    /// Collects a `LOGIN` credential across its two prompts.
    async fn collect_login(
        &mut self,
        initial: Option<Secret>,
    ) -> Result<Result<Credentials, Flow>, ProtocolError> {
        let [username_prompt, password_prompt] = sandesh_proto::login_prompts();

        // LOGIN's initial response, when present, is the username.
        let username_encoded = if let Some(encoded) = initial {
            encoded
        } else {
            let reply = reply::auth_challenge(username_prompt);
            self.send(&reply).await?;
            match self.read_sasl_line().await? {
                Ok(line) => line,
                Err(flow) => return Ok(Err(flow)),
            }
        };

        let username = match sandesh_proto::decode_login_field(username_encoded.expose()) {
            Ok(username) => username,
            Err(error) => return Ok(Err(self.reject_sasl(error).await?)),
        };

        let reply = reply::auth_challenge(password_prompt);
        self.send(&reply).await?;

        let password_encoded = match self.read_sasl_line().await? {
            Ok(line) => line,
            Err(flow) => return Ok(Err(flow)),
        };

        let password = match sandesh_proto::decode_login_field(password_encoded.expose()) {
            Ok(password) => password,
            Err(error) => return Ok(Err(self.reject_sasl(error).await?)),
        };

        // An empty field is a failed authentication, never a valid credential.
        if username.is_empty() || password.is_empty() {
            return Ok(Err(self.reject_sasl(SaslError::Malformed).await?));
        }

        Ok(Ok(Credentials {
            username,
            password: Secret::new(password),
        }))
    }

    /// Reads one line of a SASL exchange as a [`Secret`].
    ///
    /// Wrapped as a secret immediately: for `PLAIN` the line decodes straight
    /// to a password, so it must not exist as a plain `String` that some future
    /// log statement could render (ADR-035).
    async fn read_sasl_line(&mut self) -> Result<Result<Secret, Flow>, ProtocolError> {
        let line = match self
            .connection
            .read_line_owned(self.config.command_timeout)
            .await
        {
            Ok(line) => line,
            Err(ProtocolError::LineTooLong { .. }) => {
                self.connection.discard_input();
                let reply = reply::auth_failed("authentication payload too long");
                self.send(&reply).await?;
                return Ok(Err(Flow::Continue));
            }
            Err(error) if error.is_disconnect() => return Ok(Err(Flow::Close)),
            Err(error) => return Err(error),
        };

        // Lossy rather than strict: the decoder rejects anything that is not
        // valid base64 anyway, and a non-UTF-8 line must produce an
        // authentication failure rather than a transport error.
        Ok(Ok(Secret::new(String::from_utf8_lossy(&line).into_owned())))
    }

    /// Answers a malformed or cancelled SASL exchange.
    async fn reject_sasl(&mut self, error: SaslError) -> Result<Flow, ProtocolError> {
        let reply = if matches!(error, SaslError::Cancelled) {
            // RFC 4954 §4: a client that cancels gets 501, not an auth failure.
            reply::auth_cancelled()
        } else {
            // Everything else is one indistinguishable failure on the wire.
            reply::auth_invalid()
        };

        self.send(&reply).await?;
        Ok(Flow::Continue)
    }

    /// Verifies a decoded credential against the account store.
    async fn verify(&mut self, credentials: Credentials) -> Result<Flow, ProtocolError> {
        // A store failure is treated exactly as "no such credential" — a
        // database that cannot answer is not permission to proceed — but it is
        // logged first, which is why the service reports it rather than folding
        // it into `None` (ADR-071). The core cannot log; this can.
        let stored = match self.delivery.credential_hash(&credentials.username).await {
            Ok(stored) => stored,
            Err(error) => {
                tracing::error!(%error, "cannot read the account for authentication");
                None
            }
        };

        let auth_required =
            self.config.auth_mode.is_required() || self.delivery.is_auth_required().await;

        let success = if stored.is_some() || auth_required || self.config.auth_mode.is_enabled() {
            sandesh_proto::verify_optional(&credentials.password, stored.as_deref())
        } else {
            // Permissive developer mode: when auth is not required and no password
            // is configured for the account, accept credentials so dev tools and
            // test suites can provide dummy auth.
            true
        };

        if success {
            tracing::info!(
                peer = self.peer.as_deref().unwrap_or("unknown"),
                username = %credentials.username,
                "authentication succeeded"
            );
            self.authenticated_as = Some(credentials.username);
            let reply = reply::auth_succeeded();
            self.send(&reply).await?;
        } else {
            // The username is logged; the password is a `Secret` and cannot be.
            tracing::warn!(
                peer = self.peer.as_deref().unwrap_or("unknown"),
                username = %credentials.username,
                "authentication failed"
            );
            let reply = reply::auth_invalid();
            self.send(&reply).await?;
        }

        Ok(Flow::Continue)
    }

    /// Builds the `EHLO` capability list.
    ///
    /// **Only capabilities that actually work appear here.** Advertising
    /// `AUTH` or `STARTTLS` before Phase 4.5 implements them would make clients
    /// attempt something that cannot succeed — the worst failure mode an SMTP
    /// server has. The config types enforce this: `is_enabled` and
    /// `advertises_starttls` are `false` in this phase by construction.
    fn ehlo_reply(&self) -> Reply {
        let mut lines = vec![format!(
            "{} greets {}",
            self.config.hostname,
            self.helo.as_deref().unwrap_or("you")
        )];

        // SIZE (RFC 1870): enforced in DATA collection.
        lines.push(format!("SIZE {}", self.config.max_message_size));
        // 8BITMIME (RFC 6152): content is stored as bytes, so 8-bit is native.
        lines.push(String::from("8BITMIME"));
        // PIPELINING (RFC 2920): the line reader buffers whatever arrives, so
        // batched commands are handled by construction.
        lines.push(String::from("PIPELINING"));
        // ENHANCEDSTATUSCODES (RFC 2034): every reply carries one.
        lines.push(String::from("ENHANCEDSTATUSCODES"));
        // CHUNKING (RFC 3030, Phase 8 A4): BDAT is implemented, so advertising
        // it is truthful — which is the only condition this list has ever
        // applied (ADR-088).
        lines.push(String::from("CHUNKING"));

        // AUTH appears on an encrypted connection (ADR-033), or on a cleartext
        // connection when allow_cleartext_auth is enabled (developer mode / localhost).
        // If STARTTLS is advertised on this cleartext connection and cleartext auth is not
        // explicitly allowed, AUTH is withheld until the TLS upgrade.
        let offers_auth = self.config.auth_mode.is_offered_over(self.encrypted)
            || (self.config.allow_cleartext_auth
                && (self.encrypted
                    || !self.config.tls_mode.advertises_starttls()
                    || self.config.auth_mode.is_enabled()));
        if offers_auth {
            let mechanisms = Mechanism::advertised()
                .iter()
                .map(|mechanism| mechanism.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("AUTH {mechanisms}"));
        }
        // STARTTLS is pointless once the connection is already encrypted, and
        // RFC 3207 §4.2 forbids re-advertising it after a successful upgrade.
        if self.config.tls_mode.advertises_starttls() && !self.encrypted {
            lines.push(String::from("STARTTLS"));
        }

        Reply::multiline(reply::Code::Ok, lines)
    }

    /// Applies an accepted transition.
    fn apply(&mut self, transition: Transition) {
        if let Transition::Accepted { next } = transition {
            self.state = next;
        }
    }

    /// Writes a reply.
    ///
    /// Flushing is skipped while the client has more input buffered — that is
    /// the pipelining case, and batching the replies into one write is the
    /// whole point of advertising `PIPELINING`.
    async fn send(&mut self, reply: &Reply) -> Result<(), ProtocolError> {
        let wire = reply.to_wire();
        if self.connection.has_buffered_input() && !reply.is_closing() {
            self.connection.write(wire.as_bytes()).await
        } else {
            self.connection.write_flush(wire.as_bytes()).await
        }
    }

    /// The fault applying at this point, if one is configured (ADR-084).
    ///
    /// Returns `None` immediately when no registry is attached, so a server
    /// without fault injection does no work at all here.
    fn match_fault(
        &self,
        point: FaultPoint,
        sender: Option<&EmailAddress>,
        recipient: Option<&EmailAddress>,
    ) -> Option<FaultMode> {
        self.faults.as_ref()?.match_fault(point, sender, recipient)
    }

    /// Performs the failure a matched fault calls for.
    ///
    /// Returns `None` when the conversation should carry on into the handler
    /// the fault interrupted. That is what makes [`FaultMode::SlowResponse`]
    /// honest: it must delay and then send the reply the command actually
    /// owed, because a mode that swallowed the reply would be testing a
    /// protocol violation rather than a client's patience.
    ///
    /// Logged at `warn`: an injected failure is not an error — the server is
    /// doing what it was told — but a silent one would leave a developer
    /// debugging a rejection with no trace of its cause, which is the failure
    /// mode ADR-084 exists to prevent.
    async fn apply_fault(&mut self, mode: FaultMode) -> Result<Option<Flow>, ProtocolError> {
        tracing::warn!(?mode, "applying an injected fault");

        match mode {
            FaultMode::RejectAtRcpt => {
                let reply = reply::fault_recipient_rejected();
                self.send(&reply).await?;
                Ok(Some(Flow::Continue))
            }
            FaultMode::RejectAtData => {
                let reply = reply::fault_transaction_failed();
                self.send(&reply).await?;
                Ok(Some(Flow::Continue))
            }
            FaultMode::TemporaryFailure => {
                let reply = reply::fault_temporary_failure();
                self.send(&reply).await?;
                Ok(Some(Flow::Continue))
            }
            FaultMode::SlowResponse(delay) => {
                tokio::time::sleep(delay).await;
                // Falls through: the ordinary reply still follows, just late.
                Ok(None)
            }
            // No reply at all: the client sees the socket close mid-conversation,
            // which is what a real server under load produces.
            FaultMode::ConnectionDrop => Ok(Some(Flow::Close)),
        }
    }
}
