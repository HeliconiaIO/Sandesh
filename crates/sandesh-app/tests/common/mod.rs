//! Harness for whole-application tests.
//!
//! Every instance is a **real Sandesh**: a real SMTP listener, a real HTTP
//! server, a real event bus and a real SQLite database, started through the
//! same [`Application::start`] the binary uses. Nothing is stubbed, because the
//! thing these tests exist to prove is that the composition is correct.
//!
//! ## Isolation
//!
//! Each instance gets its own temporary directory (database and blobs), its own
//! bus, and **ephemeral ports** for both listeners. Tests therefore run in
//! parallel without colliding, never touch a developer's real `sandesh.db`, and
//! do not care whether 1025 or 8080 happens to be free.
//!
//! ## Determinism
//!
//! The clients here wait for a *condition* with a bounded timeout rather than
//! sleeping for a guessed duration. A test that sleeps is either slow or flaky,
//! usually both.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use sandesh_app::Application;
use sandesh_config::{AppConfig, HttpConfig, ImapConfig, SmtpConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

/// How long any wait-for-condition helper will wait before failing.
///
/// Generous enough for a loaded CI machine, short enough that a genuine hang
/// fails the test rather than the suite timeout.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// A running application under test.
pub struct TestApp {
    /// The SMTP listener's real address, when SMTP is enabled.
    pub smtp_addr: Option<SocketAddr>,
    pub imap_addr: Option<SocketAddr>,
    pub pop3_addr: Option<SocketAddr>,
    /// The HTTP listener's real address.
    pub http: SocketAddr,
    /// Cancel to shut the application down.
    pub shutdown: CancellationToken,
    /// Joined on shutdown to prove the application actually stopped.
    pub handle: tokio::task::JoinHandle<Result<(), sandesh_app::AppError>>,
    /// Kept alive so the temporary directory outlives the application.
    _dir: tempfile::TempDir,
}

impl TestApp {
    /// Starts a fully-wired application on ephemeral ports.
    pub async fn start() -> Self {
        Self::with_config(|_| {}).await
    }

    /// Starts an application, letting the caller adjust the configuration.
    pub async fn with_config(adjust: impl FnOnce(&mut AppConfig)) -> Self {
        let dir = tempfile::tempdir().expect("create temporary directory");
        let mut config = Self::config_in(&dir);
        adjust(&mut config);

        let application = Application::start(config)
            .await
            .expect("application starts");

        let smtp_addr = application.addresses().smtp;
        let imap_addr = application.addresses().imap;
        let pop3_addr = application.addresses().pop3;
        let http = application.addresses().http.expect("HTTP is bound");
        let shutdown = application.shutdown_token();
        let handle = tokio::spawn(application.run());

        let app = Self {
            smtp_addr,
            imap_addr,
            pop3_addr,
            http,
            shutdown,
            handle,
            _dir: dir,
        };
        // Readiness rather than a sleep: the test proceeds the moment the
        // server can actually answer.
        app.await_ready().await;
        app
    }

    /// A configuration isolated to a temporary directory, on ephemeral ports.
    pub fn config_in(dir: &tempfile::TempDir) -> AppConfig {
        AppConfig {
            smtp: SmtpConfig {
                bind: "127.0.0.1:0".parse().expect("valid address"),
                hostname: String::from("sandesh.test"),
                command_timeout: Duration::from_secs(5),
                data_timeout: Duration::from_secs(5),
                ..SmtpConfig::default()
            },
            // IMAP on an ephemeral port too: these tests are about SMTP and
            // HTTP, but leaving IMAP enabled proves the listeners
            // co-exist in one application rather than only in isolation.
            imap: ImapConfig {
                bind: Some("127.0.0.1:0".parse().expect("valid address")),
                hostname: String::from("sandesh.test"),
                command_timeout: Duration::from_secs(5),
                ..ImapConfig::default()
            },
            pop3: sandesh_config::Pop3Config {
                bind: Some("127.0.0.1:0".parse().expect("valid address")),
                hostname: String::from("sandesh.test"),
                command_timeout: Duration::from_secs(5),
                ..sandesh_config::Pop3Config::default()
            },
            http: HttpConfig {
                bind: "127.0.0.1:0".parse().expect("valid address"),
                ..HttpConfig::default()
            },
            database_path: dir.path().join("sandesh.db"),
            blob_path: dir.path().join("blobs"),
            log_level: String::from("warn"),
            smtp_enabled: true,
            imap_enabled: true,
            pop3_enabled: true,
            http_enabled: true,
            // Spread the defaults for everything else, so a new field does not
            // break every harness that only cares about ports and paths.
            ..AppConfig::local()
        }
    }

    /// The SMTP address, for the usual case where SMTP is enabled.
    pub fn smtp(&self) -> SocketAddr {
        self.smtp_addr.expect("SMTP is enabled in this test")
    }

    /// The IMAP address, for the usual case where IMAP is enabled.
    pub fn imap(&self) -> SocketAddr {
        self.imap_addr.expect("IMAP is enabled in this test")
    }

    /// The POP3 address, for the usual case where POP3 is enabled.
    pub fn pop3(&self) -> SocketAddr {
        self.pop3_addr.expect("POP3 is enabled in this test")
    }

    /// The base URL of the API.
    pub fn api(&self) -> String {
        format!("http://{}/api/v1", self.http)
    }

    /// Blocks until readiness reports success, or fails.
    pub async fn await_ready(&self) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if let Ok((status, _)) = self.try_get("/health/ready").await
                && status == 200
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "application never became ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Issues a GET and returns the status and body, failing on a transport error.
    pub async fn get(&self, path: &str) -> (u16, Vec<u8>) {
        self.try_get(path)
            .await
            .unwrap_or_else(|error| panic!("GET {path} failed: {error}"))
    }

    /// Issues a GET, returning a transport error rather than failing.
    pub async fn try_get(&self, path: &str) -> std::io::Result<(u16, Vec<u8>)> {
        self.request("GET", path, None).await
    }

    /// Issues a GET and parses the body as JSON.
    pub async fn get_json(&self, path: &str) -> (u16, serde_json::Value) {
        let (status, body) = self.get(path).await;
        let json = serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "GET {path} returned invalid JSON ({error}): {}",
                String::from_utf8_lossy(&body)
            )
        });
        (status, json)
    }

    /// Issues a request with an optional JSON body.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> std::io::Result<(u16, Vec<u8>)> {
        let mut stream = TcpStream::connect(self.http).await?;

        let mut request = format!(
            "{method} /api/v1{path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.http
        );
        if let Some(body) = body {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        if let Some(body) = body {
            request.push_str(body);
        }

        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        Ok(split_response(&raw))
    }

    /// Delivers a message over a real SMTP conversation.
    ///
    /// Returns the final `DATA` reply, so a caller can assert on the `250`
    /// rather than assuming acceptance.
    pub async fn deliver(&self, from: &str, to: &str, message: &str) -> String {
        let mut client = SmtpClient::connect(self.smtp()).await;
        client.expect("220").await;
        client.send("EHLO test.local").await;
        client.read_multiline().await;
        client.send(&format!("MAIL FROM:<{from}>")).await;
        client.expect("250").await;
        client.send(&format!("RCPT TO:<{to}>")).await;
        client.expect("250").await;
        client.send("DATA").await;
        client.expect("354").await;
        client.send(&format!("{message}\r\n.")).await;
        let reply = client.read_line().await;
        client.send("QUIT").await;
        reply
    }

    /// Shuts down and waits for the application to stop.
    pub async fn stop(self) {
        self.shutdown.cancel();
        let stopped = tokio::time::timeout(TIMEOUT, self.handle).await;
        let joined = stopped.expect("application stopped within the timeout");
        joined
            .expect("the application task did not panic")
            .expect("the application stopped cleanly");
    }
}

/// Splits a raw HTTP response into a status code and body.
fn split_response(raw: &[u8]) -> (u16, Vec<u8>) {
    let text = String::from_utf8_lossy(raw);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);

    // Headers end at the first blank line; everything after is the body.
    let body = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or_else(Vec::new, |index| raw[index + 4..].to_vec());

    (status, body)
}

/// A line-oriented SMTP client.
pub struct SmtpClient {
    reader: BufReader<TcpStream>,
}

impl SmtpClient {
    /// Connects to an SMTP server.
    pub async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("connect to SMTP");
        Self {
            reader: BufReader::new(stream),
        }
    }

    /// Sends one command line.
    pub async fn send(&mut self, line: &str) {
        self.reader
            .get_mut()
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("write command");
    }

    /// Reads one reply line.
    pub async fn read_line(&mut self) -> String {
        let mut line = String::new();
        tokio::time::timeout(TIMEOUT, self.reader.read_line(&mut line))
            .await
            .expect("SMTP reply arrived in time")
            .expect("read reply");
        line.trim_end().to_owned()
    }

    /// Reads a multi-line reply to completion.
    pub async fn read_multiline(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let line = self.read_line().await;
            let last = line.as_bytes().get(3) != Some(&b'-');
            lines.push(line);
            if last {
                return lines;
            }
        }
    }

    /// Reads a line and asserts it starts with the expected code.
    pub async fn expect(&mut self, code: &str) -> String {
        let line = self.read_line().await;
        assert!(line.starts_with(code), "expected {code}, got {line}");
        line
    }
}

// ---------------------------------------------------------------------------
// WebSocket client
// ---------------------------------------------------------------------------

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TMessage;

/// A real WebSocket client against `/api/v1/events`.
pub struct WsClient {
    socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
}

impl WsClient {
    /// Connects to an application's event stream.
    pub async fn connect(app: &TestApp) -> Self {
        let url = format!("ws://{}/api/v1/events", app.http);
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("websocket handshake");
        Self { socket }
    }

    /// Sends a JSON frame.
    pub async fn send(&mut self, value: serde_json::Value) {
        self.socket
            .send(TMessage::Text(value.to_string().into()))
            .await
            .expect("send frame");
    }

    /// Waits for the next JSON frame, ignoring pings and pongs.
    ///
    /// Bounded by [`TIMEOUT`]: a missing event fails the test rather than
    /// hanging it, and no test ever sleeps hoping an event has arrived.
    pub async fn next_json(&mut self) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = tokio::time::timeout(remaining, self.socket.next())
                .await
                .expect("a frame arrived before the timeout")
                .expect("the stream is still open")
                .expect("a readable frame");

            match message {
                TMessage::Text(text) => {
                    return serde_json::from_str(&text).expect("frames are JSON");
                }
                // Control frames carry no test signal.
                TMessage::Ping(_) | TMessage::Pong(_) => {}
                TMessage::Close(_) => panic!("the server closed the connection"),
                other => panic!("unexpected frame: {other:?}"),
            }
        }
    }

    /// Waits for the next frame whose `event.kind` matches.
    ///
    /// Skips the events a test is not asking about, so an assertion about
    /// `MessageReceived` is not broken by `EntryCreated` arriving first.
    pub async fn await_event(&mut self, kind: &str) -> serde_json::Value {
        loop {
            let frame = self.next_json().await;
            if frame["type"] == "event" && frame["event"]["kind"] == kind {
                return frame;
            }
        }
    }

    /// Closes the connection.
    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}
