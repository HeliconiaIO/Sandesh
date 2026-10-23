//! Transcript capture through a real `Connection` (ADR-066).
//!
//! The unit tests in `transcript.rs` cover the recorder in isolation. These
//! drive an actual `Connection` over an in-memory duplex, which is what proves
//! the recorder is wired into the read and write paths rather than merely
//! being correct on its own.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use sandesh_proto::{Connection, Transcript};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn reads_and_writes_are_captured_in_order() {
    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    let transcript = Transcript::recording();
    let mut connection = Connection::with_transcript(read, write, 4096, None, transcript.clone());

    client.write_all(b"EHLO localhost\r\n").await.unwrap();
    let line = connection.read_line_owned(TIMEOUT).await.expect("read");
    assert_eq!(line, b"EHLO localhost");

    connection
        .write_flush(b"250 sandesh.test\r\n")
        .await
        .expect("write");

    let (lines, truncated) = transcript.snapshot();
    assert!(!truncated);
    assert_eq!(
        lines,
        vec![
            String::from("C: EHLO localhost"),
            String::from("S: 250 sandesh.test"),
        ],
        "the conversation must read in the order it happened"
    );
}

#[tokio::test]
async fn a_multi_line_reply_is_split_into_protocol_lines() {
    // An EHLO response is one write carrying many lines. Recording it as a
    // single blob would make the transcript unreadable exactly where it is
    // most useful.
    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    let transcript = Transcript::recording();
    let mut connection = Connection::with_transcript(read, write, 4096, None, transcript.clone());

    connection
        .write_flush(b"250-sandesh.test\r\n250-SIZE 1024\r\n250 PIPELINING\r\n")
        .await
        .expect("write");

    // Drain, so the duplex does not block on a full buffer.
    let mut sink = vec![0_u8; 128];
    let _ = client.read(&mut sink).await;

    let (lines, _) = transcript.snapshot();
    assert_eq!(lines.len(), 3, "one entry per protocol line: {lines:?}");
    assert!(lines[1].contains("SIZE 1024"), "{lines:?}");
}

#[tokio::test]
async fn a_credential_sent_over_the_wire_never_reaches_the_transcript() {
    // The property that matters most: this is a live connection, and the
    // password is real input, not a string handed to the redactor directly.
    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    let transcript = Transcript::recording();
    let mut connection = Connection::with_transcript(read, write, 4096, None, transcript.clone());

    for line in [
        &b"USER alice\r\n"[..],
        &b"PASS hunter2\r\n"[..],
        &b"a001 LOGIN alice hunter2\r\n"[..],
        &b"AUTH PLAIN AGFsaWNlAGh1bnRlcjI=\r\n"[..],
    ] {
        client.write_all(line).await.unwrap();
        connection.read_line_owned(TIMEOUT).await.expect("read");
    }

    let text = transcript.snapshot().0.join("\n");
    assert!(
        !text.contains("hunter2"),
        "a password reached the transcript: {text}"
    );
    assert!(
        !text.contains("AGFsaWNlAGh1bnRlcjI="),
        "a base64 credential reached the transcript: {text}"
    );
    // The commands themselves are still visible: redaction removes the secret,
    // not the diagnostic value.
    assert!(text.contains("USER alice"), "{text}");
    assert!(text.contains("PASS [redacted]"), "{text}");
}

#[tokio::test]
async fn a_counted_literal_is_recorded_by_size_only() {
    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    let transcript = Transcript::recording();
    let mut connection = Connection::with_transcript(read, write, 4096, None, transcript.clone());

    let body = b"Subject: secret\r\n\r\nthe one-time code is 123456\r\n";
    client.write_all(body).await.unwrap();
    let read_back = connection
        .read_exact_owned(body.len(), 8192, TIMEOUT)
        .await
        .expect("read literal");
    assert_eq!(read_back, body, "the protocol still gets the real bytes");

    let text = transcript.snapshot().0.join("\n");
    assert!(
        !text.contains("123456"),
        "message content must never be transcribed: {text}"
    );
    assert!(
        text.contains(&format!("literal: {} bytes not recorded", body.len())),
        "{text}"
    );
}

#[tokio::test]
async fn a_large_outbound_body_is_elided_rather_than_transcribed() {
    // `RETR` and `FETCH BODY[]` serve message content through the same write
    // path a reply uses. Size is the only thing that distinguishes them.
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(server);

    let transcript = Transcript::recording();
    let mut connection = Connection::with_transcript(read, write, 4096, None, transcript.clone());

    let mut body = b"Subject: big\r\n\r\n".to_vec();
    body.extend(std::iter::repeat_n(b'x', 8192));
    body.extend_from_slice(b"\r\nthe one-time code is 123456\r\n");

    connection.write_flush(&body).await.expect("write");

    let mut sink = vec![0_u8; body.len()];
    let _ = client.read_exact(&mut sink).await;

    let text = transcript.snapshot().0.join("\n");
    assert!(
        !text.contains("123456"),
        "a served body must not be transcribed: {text}"
    );
    assert!(text.contains("body:"), "{text}");
}

#[tokio::test]
async fn a_connection_built_without_a_transcript_records_nothing() {
    // The default path, and the one every existing test uses. It must stay
    // free: no buffer, no allocation, no capture.
    let (mut client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(server);

    let mut connection = Connection::new(read, write, 4096, None);
    client.write_all(b"NOOP\r\n").await.unwrap();
    connection.read_line_owned(TIMEOUT).await.expect("read");

    assert!(!connection.transcript().is_recording());
    assert_eq!(connection.transcript().snapshot(), (Vec::new(), false));
}

#[tokio::test]
async fn two_connections_sharing_a_transcript_produce_one_conversation() {
    // The STARTTLS shape: the cleartext session is discarded and a second
    // `Connection` runs over the upgraded transport. One socket, one story.
    let transcript = Transcript::recording();

    {
        let (mut client, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(server);
        let mut cleartext =
            Connection::with_transcript(read, write, 4096, None, transcript.clone());

        client.write_all(b"STARTTLS\r\n").await.unwrap();
        cleartext.read_line_owned(TIMEOUT).await.expect("read");
    }

    transcript.note("TLS established");

    {
        let (mut client, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(server);
        let mut encrypted =
            Connection::with_transcript(read, write, 4096, None, transcript.clone());

        client.write_all(b"EHLO localhost\r\n").await.unwrap();
        encrypted.read_line_owned(TIMEOUT).await.expect("read");
    }

    let (lines, _) = transcript.snapshot();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("STARTTLS"));
    assert!(lines[1].contains("TLS established"));
    assert!(
        lines[2].contains("EHLO"),
        "the post-upgrade session appends to the same transcript: {lines:?}"
    );
}
