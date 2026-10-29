//! SMTP protocol behaviour over TCP: sequencing, replies, framing, limits.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, code_of, fixture};
use sandesh_config::SmtpConfig;
use sandesh_core::{DeliveryStore, MessageStore};
use std::time::Duration;

#[tokio::test]
async fn greeting_is_220_and_names_the_host() {
    let server = TestServer::start().await;
    let client = server.connect().await;

    assert_eq!(code_of(&client.greeting), 220);
    assert!(
        client.greeting.contains("sandesh.test"),
        "greeting should name the host: {}",
        client.greeting
    );
}

#[tokio::test]
async fn ehlo_advertises_only_working_capabilities() {
    // The rule that matters most: never advertise what is not implemented.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO localhost").await;
    assert_eq!(code_of(&ehlo), 250);

    for expected in [
        "SIZE",
        "8BITMIME",
        "PIPELINING",
        "ENHANCEDSTATUSCODES",
        // CHUNKING moved from the absent list below in Phase 8 A4, when BDAT
        // was implemented (ADR-088). The move is the point: this test encodes
        // "never advertise what is not implemented", so the capability changes
        // sides exactly when its implementation lands, and not before.
        "CHUNKING",
    ] {
        assert!(ehlo.contains(expected), "{expected} missing from: {ehlo}");
    }
    // Still unimplemented, so still absent. The rule is unchanged; only
    // CHUNKING's membership changed, because only CHUNKING was built.
    for absent in ["AUTH", "STARTTLS", "SMTPUTF8", "DSN"] {
        assert!(
            !ehlo.contains(absent),
            "{absent} is not implemented and must not be advertised: {ehlo}"
        );
    }
}

#[tokio::test]
async fn ehlo_size_matches_the_configured_limit() {
    let config = SmtpConfig {
        max_message_size: 12345,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO localhost").await;
    assert!(
        ehlo.contains("SIZE 12345"),
        "advertised SIZE must be the real limit: {ehlo}"
    );
}

#[tokio::test]
async fn multiline_ehlo_is_correctly_framed() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    let ehlo = client.send("EHLO localhost").await;

    let lines: Vec<&str> = ehlo.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines.len() > 1, "EHLO should be multi-line");

    for line in &lines[..lines.len() - 1] {
        assert_eq!(line.as_bytes().get(3), Some(&b'-'), "continuation: {line}");
    }
    let last = lines[lines.len() - 1];
    assert_eq!(last.as_bytes().get(3), Some(&b' '), "final line: {last}");
}

#[tokio::test]
async fn helo_works_as_a_legacy_greeting() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;

    let helo = client.send("HELO localhost").await;
    assert_eq!(code_of(&helo), 250);
    // HELO must be single-line: no capability list.
    assert_eq!(helo.lines().filter(|l| !l.is_empty()).count(), 1);

    let reply = client
        .deliver(
            "a@example.test",
            &["bob@example.test"],
            b"Subject: x\r\n\r\ny\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250, "HELO sessions can still deliver");
}

#[tokio::test]
async fn commands_are_case_insensitive() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;

    assert_eq!(code_of(&client.send("ehlo localhost").await), 250);
    assert_eq!(
        code_of(&client.send("MaIl FrOm:<a@example.test>").await),
        250
    );
    assert_eq!(
        code_of(&client.send("rCpT tO:<bob@example.test>").await),
        250
    );
    assert_eq!(code_of(&client.send("DaTa").await), 354);
    assert_eq!(
        code_of(&client.send_data(b"Subject: x\r\n\r\ny\r\n").await),
        250
    );
    assert_eq!(code_of(&client.send("nOoP").await), 250);
    assert_eq!(code_of(&client.send("qUiT").await), 221);
}

#[tokio::test]
async fn invalid_sequences_are_rejected_with_503() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // Before greeting.
    assert_eq!(
        code_of(&client.send("MAIL FROM:<a@example.test>").await),
        503
    );
    assert_eq!(code_of(&client.send("RCPT TO:<b@example.test>").await), 503);
    assert_eq!(code_of(&client.send("DATA").await), 503);

    client.send("EHLO localhost").await;

    // Greeted, but no transaction.
    assert_eq!(code_of(&client.send("RCPT TO:<b@example.test>").await), 503);
    assert_eq!(code_of(&client.send("DATA").await), 503);

    // Transaction open, no recipients.
    client.send("MAIL FROM:<a@example.test>").await;
    assert_eq!(
        code_of(&client.send("DATA").await),
        503,
        "DATA needs a recipient"
    );
    assert_eq!(
        code_of(&client.send("MAIL FROM:<c@example.test>").await),
        503,
        "nested MAIL is invalid"
    );
}

#[tokio::test]
async fn rejections_do_not_close_the_connection() {
    // A protocol error is recoverable: the session continues.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;

    assert_eq!(code_of(&client.send("DATA").await), 503);
    assert_eq!(code_of(&client.send("NONSENSE").await), 500);
    assert_eq!(code_of(&client.send("MAIL FROM:").await), 501);

    // Still usable.
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "a@example.test",
            &["bob@example.test"],
            b"Subject: x\r\n\r\ny\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250, "the session survived the errors");
}

#[tokio::test]
async fn unknown_and_unimplemented_commands_get_different_codes() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    assert_eq!(code_of(&client.send("FLURBLE").await), 500, "unknown verb");
    assert_eq!(
        code_of(&client.send("AUTH PLAIN dGVzdA==").await),
        502,
        "recognised but not implemented"
    );
    assert_eq!(code_of(&client.send("STARTTLS").await), 502);
}

#[tokio::test]
async fn vrfy_does_not_disclose_addresses() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let known = client.send("VRFY bob@example.test").await;
    let unknown = client.send("VRFY nobody@example.test").await;

    assert_eq!(code_of(&known), 252);
    assert_eq!(
        code_of(&known),
        code_of(&unknown),
        "VRFY must not reveal which addresses exist"
    );
}

#[tokio::test]
async fn noop_and_help_work_in_any_state() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    assert_eq!(code_of(&client.send("NOOP").await), 250, "before greeting");
    assert_eq!(code_of(&client.send("HELP").await), 250);

    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<a@example.test>").await;
    assert_eq!(code_of(&client.send("NOOP").await), 250, "mid-transaction");
}

#[tokio::test]
async fn dot_stuffing_round_trips_through_the_server() {
    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    // Content with lines that require stuffing on the wire.
    let content = b"Subject: dots\r\n\r\n.hidden\r\n..double\r\nnormal\r\n";
    let reply = client
        .deliver("a@example.test", &["bob@example.test"], content)
        .await;
    assert_eq!(code_of(&reply), 250);

    let ids = server.stored_messages().await;
    let raw = server
        .messages
        .raw(ids[0])
        .await
        .expect("query")
        .expect("stored");

    assert_eq!(
        raw.bytes(),
        content.as_slice(),
        "unstuffing must exactly invert the client's stuffing"
    );
    let _ = bob;
}

#[tokio::test]
async fn the_terminator_is_never_stored() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let content = b"Subject: end\r\n\r\nlast line\r\n";
    client
        .deliver("a@example.test", &["bob@example.test"], content)
        .await;

    let ids = server.stored_messages().await;
    let raw = server
        .messages
        .raw(ids[0])
        .await
        .expect("query")
        .expect("stored");

    assert!(
        !raw.bytes().ends_with(b"\r\n.\r\n"),
        "the SMTP terminator is framing, not content"
    );
    assert_eq!(raw.bytes(), content.as_slice());
}

#[tokio::test]
async fn every_fixture_survives_smtp_transport_byte_for_byte() {
    // The raw-fidelity guarantee, across the whole corpus.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let mut sent: Vec<(String, Vec<u8>)> = Vec::new();
    for name in [
        "valid/plain-text.eml",
        "valid/html.eml",
        "valid/multipart-alternative.eml",
        "valid/multipart-mixed.eml",
        "valid/nested-multipart.eml",
        "valid/attachment.eml",
        "valid/inline-image.eml",
        "valid/multiple-attachments.eml",
        "valid/custom-headers.eml",
        "encodings/base64-body.eml",
        "encodings/encoded-headers.eml",
        "encodings/unicode.eml",
        "encodings/iso-8859-1.eml",
        "malformed/duplicate-headers.eml",
        "malformed/invalid-content-type.eml",
        "malformed/missing-mime-version.eml",
        "malformed/no-headers.eml",
        "malformed/truncated-base64.eml",
        "malformed/unterminated-boundary.eml",
        "malformed/eight-bit-header.eml",
    ] {
        let content = fixture(name);
        let reply = client
            .deliver("a@example.test", &["bob@example.test"], &content)
            .await;
        assert_eq!(code_of(&reply), 250, "{name} was rejected: {reply}");
        sent.push((name.to_owned(), content));
    }

    let ids = server.stored_messages().await;
    assert_eq!(ids.len(), sent.len());

    for (id, (name, expected)) in ids.iter().zip(&sent) {
        let raw = server
            .messages
            .raw(*id)
            .await
            .expect("query")
            .expect("stored");
        assert_eq!(
            raw.bytes(),
            expected.as_slice(),
            "{name}: stored bytes differ from what was sent"
        );
    }
}

#[tokio::test]
async fn malformed_messages_are_accepted() {
    // Sandesh is a testing tool: a malformed body must not be refused.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    for name in [
        "malformed/unterminated-boundary.eml",
        "malformed/no-headers.eml",
        "malformed/invalid-content-type.eml",
    ] {
        let reply = client
            .deliver("a@example.test", &["bob@example.test"], &fixture(name))
            .await;
        assert_eq!(code_of(&reply), 250, "{name} must be accepted: {reply}");
    }
}

#[tokio::test]
async fn binary_content_survives_transport() {
    // Message content is bytes, not UTF-8.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let mut content = b"Subject: binary\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
    // Every byte except CR and LF, which are framing.
    content.extend((0u8..=255).filter(|b| *b != b'\r' && *b != b'\n'));
    content.extend_from_slice(b"\r\n");

    let reply = client
        .deliver("a@example.test", &["bob@example.test"], &content)
        .await;
    assert_eq!(code_of(&reply), 250);

    let ids = server.stored_messages().await;
    let raw = server
        .messages
        .raw(ids[0])
        .await
        .expect("query")
        .expect("stored");
    assert_eq!(
        raw.bytes(),
        content.as_slice(),
        "arbitrary bytes must survive"
    );
}

#[tokio::test]
async fn oversized_message_is_rejected_with_552() {
    let config = SmtpConfig {
        max_message_size: 1024,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let mut content = b"Subject: big\r\n\r\n".to_vec();
    content.extend(std::iter::repeat_n(b'x', 4096));
    content.extend_from_slice(b"\r\n");

    let reply = client
        .deliver("a@example.test", &["bob@example.test"], &content)
        .await;
    assert_eq!(code_of(&reply), 552, "oversize must be refused: {reply}");
    assert_eq!(server.message_count().await, 0, "nothing is stored");

    // The refusal is itself a routing outcome the Delivery view has to explain:
    // "the message never arrived" is the question it exists to answer, and a
    // view that lists only what worked cannot answer it (Phase 7, A1).
    let recorded = server.deliveries.recent(10).await.expect("recent");
    let rejection = recorded
        .iter()
        .find(|d| d.message_id.is_none())
        .expect("an oversize refusal must leave a delivery record");

    assert_eq!(
        rejection.outcomes.len(),
        1,
        "the record keeps the envelope's recipients"
    );
    assert_eq!(
        rejection.outcomes[0].outcome,
        sandesh_core::DeliveryOutcome::Rejected
    );
    assert!(
        rejection.outcomes[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("1024")),
        "the reason must name the limit that was exceeded: {:?}",
        rejection.outcomes[0].reason
    );
    assert_eq!(
        rejection
            .envelope
            .mail_from
            .as_ref()
            .map(|a| a.normalized()),
        Some(String::from("a@example.test")),
        "the envelope sender is preserved on a refusal"
    );

    // The session survives and can still deliver a small message.
    let ok = client
        .deliver(
            "a@example.test",
            &["bob@example.test"],
            b"Subject: small\r\n\r\nx\r\n",
        )
        .await;
    assert_eq!(code_of(&ok), 250, "the connection stays usable: {ok}");
    assert_eq!(server.message_count().await, 1);
}

#[tokio::test]
async fn declared_size_over_the_limit_is_refused_before_data() {
    // SIZE is advertised, so the declaration must be honoured (RFC 1870).
    let config = SmtpConfig {
        max_message_size: 1024,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let reply = client.send("MAIL FROM:<a@example.test> SIZE=999999").await;
    assert_eq!(code_of(&reply), 552, "refuse early rather than after DATA");
}

#[tokio::test]
async fn oversized_command_line_is_rejected_without_dropping_the_session() {
    let config = SmtpConfig {
        max_line_length: 512,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let huge = format!("MAIL FROM:<{}@example.test>", "x".repeat(2000));
    let reply = client.send(&huge).await;
    assert_eq!(code_of(&reply), 501, "oversized line: {reply}");

    // Still usable afterwards.
    let ok = client
        .deliver(
            "a@example.test",
            &["bob@example.test"],
            b"Subject: x\r\n\r\ny\r\n",
        )
        .await;
    assert_eq!(code_of(&ok), 250);
}

#[tokio::test]
async fn recipient_limit_is_enforced() {
    let config = SmtpConfig {
        max_recipients: 3,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    server.catch_all().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<a@example.test>").await;

    for n in 1..=3 {
        let reply = client.send(&format!("RCPT TO:<r{n}@example.test>")).await;
        assert_eq!(code_of(&reply), 250, "recipient {n}");
    }

    let over = client.send("RCPT TO:<r4@example.test>").await;
    assert_eq!(code_of(&over), 452, "the limit must be enforced: {over}");
}

#[tokio::test]
async fn strict_policy_rejects_unknown_recipients() {
    let config = SmtpConfig {
        unknown_recipient: sandesh_config::UnknownRecipientPolicy::Strict,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<a@example.test>").await;

    assert_eq!(
        code_of(&client.send("RCPT TO:<bob@example.test>").await),
        250
    );

    let unknown = client.send("RCPT TO:<nobody@example.test>").await;
    assert_eq!(code_of(&unknown), 550, "strict must refuse: {unknown}");

    // The transaction survives: the known recipient still receives.
    assert_eq!(code_of(&client.send("DATA").await), 354);
    assert_eq!(
        code_of(&client.send_data(b"Subject: x\r\n\r\ny\r\n").await),
        250
    );
    assert_eq!(server.entry_count().await, 1);
}

#[tokio::test]
async fn idle_connections_time_out() {
    let config = SmtpConfig {
        command_timeout: Duration::from_millis(300),
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    let mut client = server.connect().await;

    // Say nothing and wait for the server to give up.
    let reply = client.read_reply().await;
    assert_eq!(
        code_of(&reply),
        421,
        "idle sessions must be released: {reply}"
    );
}

#[tokio::test]
async fn unsupported_parameters_are_refused_with_504() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    // SMTPUTF8 is not advertised, so it must not be silently accepted.
    let reply = client.send("MAIL FROM:<a@example.test> SMTPUTF8").await;
    assert_eq!(code_of(&reply), 504, "{reply}");
}

/// What nodemailer sends for `to: "jöran@app.test"` against a server that does
/// not advertise SMTPUTF8. The reply must name the real problem, and the
/// session must survive it so the client can carry on with other recipients.
#[tokio::test]
async fn a_non_ascii_address_is_refused_with_553_not_500() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let reply = client.send("MAIL FROM:<jöran@example.test>").await;
    assert_eq!(code_of(&reply), 553, "{reply}");
    assert!(
        reply.contains("5.6.7") && reply.contains("SMTPUTF8"),
        "{reply}"
    );

    let reply = client.send("MAIL FROM:<a@example.test>").await;
    assert_eq!(code_of(&reply), 250, "{reply}");
    let reply = client.send("RCPT TO:<jöran@example.test>").await;
    assert_eq!(code_of(&reply), 553, "{reply}");
    let reply = client.send("RCPT TO:<dev@example.test>").await;
    assert_eq!(code_of(&reply), 250, "{reply}");
}
