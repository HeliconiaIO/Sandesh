//! `SANDESH_MAX_MESSAGE_SIZE`, tested by running the real binary.
//!
//! The limit is read from the environment in `sandesh_app::env::apply`, which
//! reads the process environment directly. Setting variables in-process is
//! `unsafe` under edition 2024, so the only honest test is the one a user runs:
//! start the binary with the variable set and ask the SMTP listener what it
//! advertises.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sandesh"))
}

/// An ephemeral loopback address the OS reports free.
fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").to_string()
}

/// The binary with `SANDESH_*` scrubbed and every listener on a free port.
fn command(data_dir: &std::path::Path, smtp: &str) -> Command {
    let mut command = Command::new(binary());
    for (key, _) in std::env::vars() {
        if key.starts_with("SANDESH_") {
            command.env_remove(key);
        }
    }
    command
        .env("SANDESH_DATA_DIR", data_dir)
        .env("SANDESH_SMTP_ADDR", smtp)
        .env("SANDESH_HTTP_ADDR", free_addr())
        .env("SANDESH_IMAP_ADDR", free_addr())
        .env("SANDESH_POP3_ADDR", free_addr())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// Kills the server even when an assertion fails first.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The `EHLO` reply, once the listener accepts.
fn ehlo(smtp: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    let stream = loop {
        match TcpStream::connect(smtp) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => panic!("SMTP never came up on {smtp}: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    writer.write_all(b"EHLO test\r\n").expect("send EHLO");

    let mut reply = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("EHLO line");
        reply.push_str(&line);
        if line.len() < 4 || line.as_bytes()[3] != b'-' {
            return reply;
        }
    }
}

#[test]
fn the_variable_sets_the_advertised_smtp_size() {
    let data = tempfile::tempdir().expect("tempdir");
    let smtp = free_addr();
    let _server = Running(
        command(data.path(), &smtp)
            .env("SANDESH_MAX_MESSAGE_SIZE", "52428800")
            .spawn()
            .expect("start sandesh"),
    );

    let reply = ehlo(&smtp);
    assert!(reply.contains("SIZE 52428800"), "{reply}");
}

#[test]
fn a_nonsense_value_stops_startup_and_names_the_variable() {
    let data = tempfile::tempdir().expect("tempdir");
    let mut server = Running(
        command(data.path(), &free_addr())
            .env("SANDESH_MAX_MESSAGE_SIZE", "50MB")
            .spawn()
            .expect("start sandesh"),
    );

    // A regression that ignores the variable starts a server that never
    // exits, so wait with a deadline rather than for the process.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = server.0.try_wait().expect("poll") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "sandesh started despite an unparseable SANDESH_MAX_MESSAGE_SIZE"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(!status.success(), "a bad limit must not start");
    let mut stderr = String::new();
    std::io::Read::read_to_string(server.0.stderr.as_mut().expect("piped stderr"), &mut stderr)
        .expect("read stderr");
    assert!(stderr.contains("SANDESH_MAX_MESSAGE_SIZE"), "{stderr}");
}
