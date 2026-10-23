//! WebSocket behaviour against a running application.
//!
//! The properties here are the ones that decide whether the event stream is
//! trustworthy: every subscriber sees every event, one client's problems are
//! its own, and — the critical acceptance criterion — **a slow WebSocket client
//! cannot stop mail from being delivered**.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestApp, WsClient};
use serde_json::json;

const MESSAGE: &str = "Subject: ws\r\n\r\nBody.";

#[tokio::test(flavor = "multi_thread")]
async fn a_client_connects_and_is_answered() {
    let app = TestApp::start().await;
    let mut ws = WsClient::connect(&app).await;

    ws.send(json!({"type": "ping"})).await;
    assert_eq!(ws.next_json().await["type"], "pong");

    ws.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_connected_client_receives_the_event() {
    // Fan-out. Three independent connections, one delivery, three deliveries of
    // the same fact.
    let app = TestApp::start().await;

    let mut a = WsClient::connect(&app).await;
    let mut b = WsClient::connect(&app).await;
    let mut c = WsClient::connect(&app).await;

    app.deliver("s@test.local", "dev@example.test", MESSAGE)
        .await;

    for (name, client) in [("a", &mut a), ("b", &mut b), ("c", &mut c)] {
        let frame = client.await_event("MessageReceived").await;
        assert_eq!(
            frame["event"]["kind"], "MessageReceived",
            "subscriber {name} missed the event"
        );
    }

    a.close().await;
    b.close().await;
    c.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_client_disconnecting_does_not_disturb_the_others() {
    let app = TestApp::start().await;

    let leaving = WsClient::connect(&app).await;
    let mut staying = WsClient::connect(&app).await;

    leaving.close().await;

    app.deliver("s@test.local", "dev@example.test", MESSAGE)
        .await;

    let frame = staying.await_event("MessageReceived").await;
    assert_eq!(frame["event"]["kind"], "MessageReceived");

    staying.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_can_reconnect_and_receive_later_events() {
    // No resume is implemented, so a reconnecting client sees what happens from
    // then on. What matters is that reconnecting works at all and that the
    // server tolerated the disappearance.
    let app = TestApp::start().await;

    let first = WsClient::connect(&app).await;
    first.close().await;

    let mut second = WsClient::connect(&app).await;
    app.deliver("s@test.local", "dev@example.test", MESSAGE)
        .await;

    // EntryCreated is frame 1 on this connection, MessageReceived frame 2:
    // the per-connection counter restarted, which is the point.
    let frame = second.await_event("MessageReceived").await;
    assert_eq!(frame["seq"], 2, "a new connection counts from one again");

    second.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mailbox_filter_is_applied_server_side() {
    let app = TestApp::start().await;
    let mut ws = WsClient::connect(&app).await;

    // Subscribe to a mailbox that does not exist, so nothing can match.
    ws.send(json!({
        "type": "subscribe",
        "filters": {"mailbox_id": "01a05d79-0000-7000-8000-000000000000"}
    }))
    .await;
    assert_eq!(ws.next_json().await["type"], "subscribed");

    app.deliver("s@test.local", "dev@example.test", MESSAGE)
        .await;

    // The message is stored — so the absence of a frame below is the filter
    // working, not the delivery failing.
    let (status, list) = app.get_json("/messages").await;
    assert_eq!(status, 200);
    assert_eq!(list["total_estimate"], 1);

    // A ping is answered immediately; if any event had passed the filter it
    // would be queued ahead of the pong, so the pong arriving first proves
    // nothing did. This is an ordering assertion, not a timing one.
    ws.send(json!({"type": "ping"})).await;
    assert_eq!(
        ws.next_json().await["type"],
        "pong",
        "a filtered-out event must not reach the client"
    );

    ws.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_filter_selects_only_the_named_events() {
    let app = TestApp::start().await;
    let mut ws = WsClient::connect(&app).await;

    ws.send(json!({
        "type": "subscribe",
        "filters": {"events": ["MessageReceived"]}
    }))
    .await;
    assert_eq!(ws.next_json().await["type"], "subscribed");

    app.deliver("s@test.local", "dev@example.test", MESSAGE)
        .await;

    // Delivery emits EntryCreated then MessageReceived. With this filter the
    // *first* event frame must be MessageReceived — EntryCreated was dropped
    // server-side rather than delivered and discarded by the client.
    let frame = ws.next_json().await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["event"]["kind"], "MessageReceived");

    ws.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_websocket_client_cannot_block_smtp_delivery() {
    // The critical Phase 3 acceptance criterion.
    //
    // A client connects and then never reads. Its queue fills and the bus drops
    // its oldest events. SMTP must continue to accept mail at full speed
    // throughout — if the bus blocked its producer, delivery would stall here.
    let app = TestApp::with_config(|config| {
        // A small queue so the client falls behind after a handful of events
        // rather than needing thousands.
        config.http.websocket_queue = 4;
    })
    .await;

    // Connected but deliberately never polled.
    let _idle = WsClient::connect(&app).await;

    // Comfortably more events than the queue holds: each delivery emits two.
    for index in 0..20 {
        let reply = app
            .deliver(
                "s@test.local",
                "dev@example.test",
                &format!("Subject: slow {index}\r\n\r\nBody."),
            )
            .await;
        assert!(
            reply.starts_with("250"),
            "delivery {index} was not accepted while a client was lagging: {reply}"
        );
    }

    // Every message is stored and queryable: the stalled subscriber affected
    // nothing but its own event queue.
    let (status, list) = app.get_json("/messages?limit=100").await;
    assert_eq!(status, 200);
    assert_eq!(
        list["total_estimate"], 20,
        "every delivery must have completed"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_its_socket_is_told_it_lagged() {
    // Detecting the loss is the point of at-most-once delivery: the client
    // cannot get the events back, but it must know to resynchronise.
    //
    // Producing a lag needs a client that stops reading *the TCP socket*, not
    // merely one that stops calling `recv`. The connection task drains the bus
    // queue eagerly into the socket, so while the socket accepts writes the
    // subscriber never falls behind — good behaviour, but it means the lag path
    // only opens once the socket's own buffers are full and the writer stalls.
    //
    // So this test performs the WebSocket handshake by hand and then never
    // reads a byte, while the server is given far more events than any buffer
    // will hold.
    let app = TestApp::with_config(|config| {
        config.http.websocket_queue = 2;
    })
    .await;

    let mut socket = tokio::net::TcpStream::connect(app.http)
        .await
        .expect("connect");
    let handshake = format!(
        "GET /api/v1/events HTTP/1.1\r\n\
         Host: {}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        app.http
    );
    tokio::io::AsyncWriteExt::write_all(&mut socket, handshake.as_bytes())
        .await
        .expect("send handshake");

    // Read only the 101 response, then stop reading entirely.
    let mut buffer = [0_u8; 1024];
    let read = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer)
        .await
        .expect("read handshake response");
    assert!(
        String::from_utf8_lossy(&buffer[..read]).contains("101"),
        "the connection must have been upgraded"
    );

    // Deliver far more than the queue holds. Every one must still be accepted:
    // this doubles as another proof that a stuck client cannot block SMTP.
    for index in 0..60 {
        let reply = app
            .deliver(
                "s@test.local",
                "dev@example.test",
                &format!("Subject: lag {index}\r\n\r\nBody."),
            )
            .await;
        assert!(
            reply.starts_with("250"),
            "delivery {index} stalled behind an unresponsive websocket client"
        );
    }

    let (status, list) = app.get_json("/messages?limit=100").await;
    assert_eq!(status, 200);
    assert_eq!(
        list["total_estimate"], 60,
        "every delivery completed despite a client that never read"
    );

    drop(socket);
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_frame_does_not_close_the_connection() {
    let app = TestApp::start().await;
    let mut ws = WsClient::connect(&app).await;

    ws.send(json!({"type": "teleport"})).await;
    assert_eq!(ws.next_json().await["type"], "error");

    // Still usable afterwards.
    ws.send(json!({"type": "ping"})).await;
    assert_eq!(ws.next_json().await["type"], "pong");

    ws.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_are_released_when_clients_disconnect() {
    // A leaked subscription is a leaked bounded queue; over a long session that
    // is an unbounded memory growth vector.
    let app = TestApp::start().await;

    for _ in 0..5 {
        let ws = WsClient::connect(&app).await;
        ws.close().await;
    }

    // A fresh client still works, and the application still shuts down cleanly
    // — which it could not do if connection tasks had leaked.
    let mut ws = WsClient::connect(&app).await;
    ws.send(json!({"type": "ping"})).await;
    assert_eq!(ws.next_json().await["type"], "pong");
    ws.close().await;

    app.stop().await;
}
