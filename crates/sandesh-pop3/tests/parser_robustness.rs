//! The POP3 command parser against hostile input (TESTING_STRATEGY §9).
//!
//! The mandated property is "never panics". `Pop3Command::parse` is the first
//! code to touch bytes from an unauthenticated socket, so a panic there is a
//! remote denial of service reachable before any credential is presented.
//!
//! TESTING_STRATEGY §9 asks for this under `cargo-fuzz`, run nightly. No
//! `fuzz/` directory exists yet for any protocol, and standing up a nightly
//! libFuzzer harness is shared infrastructure rather than POP3 work, so this
//! covers the same property deterministically in the ordinary test suite: a
//! seeded generator, reproducible from its seed, that needs no nightly
//! toolchain and runs on every commit. The `cargo-fuzz` target should still be
//! added — it explores far more state than this does — but the invariant is no
//! longer unasserted while that waits.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_pop3::Pop3Command;

/// A tiny xorshift PRNG, so the corpus is random-looking but reproducible from
/// its seed without pulling in a dependency.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Verbs, separators and arguments chosen to land near the parser's edges.
const VERBS: &[&str] = &[
    "USER", "PASS", "STAT", "LIST", "RETR", "DELE", "NOOP", "RSET", "QUIT", "TOP", "UIDL", "CAPA",
    "STLS", "AUTH", "user", "ToP", "", "FOOBAR", ".", "..", "\u{1}",
];

const SEPARATORS: &[&str] = &[" ", "  ", "\t", "", " \t ", "\r", "\n"];

const ARGUMENTS: &[&str] = &[
    "",
    "1",
    "0",
    "-1",
    "abc",
    "18446744073709551615",
    "18446744073709551616",
    "99999999999999999999999999",
    "1 10",
    "1 -10",
    "1 abc",
    "+1",
    " 1",
    "1 ",
    "PLAIN",
    "PLAIN dGVzdA==",
    "PLAIN !!!!not base64!!!!",
    "\u{0}",
    "\u{7f}",
    "café",
    "🙂",
    "a b c d e f",
];

const TERMINATORS: &[&str] = &["\r\n", "\n", "\r", "", "\r\n\r\n", "\n\r"];

#[test]
fn parser_never_panics_on_generated_input() {
    // A fixed seed: a failure here reproduces exactly, and a change to the
    // generator is a deliberate edit rather than a flake.
    let mut rng = Rng(0x5EED_1939_5EED_1939);

    for _ in 0..20_000 {
        let mut line = String::new();
        line.push_str(rng.pick(VERBS));
        line.push_str(rng.pick(SEPARATORS));
        line.push_str(rng.pick(ARGUMENTS));
        line.push_str(rng.pick(TERMINATORS));

        // The contract is only that this returns rather than unwinding; both
        // Ok and Err are acceptable outcomes for arbitrary bytes.
        let _ = Pop3Command::parse(&line);
    }
}

#[test]
fn parser_never_panics_on_arbitrary_bytes() {
    // The generator above stays near the grammar. This one does not: it feeds
    // lossy-decoded random bytes, including control characters and partial
    // UTF-8, which is closer to what a scanner actually sends.
    let mut rng = Rng(0xBAD_C0FFEE);

    for _ in 0..20_000 {
        let len = rng.below(64);
        let bytes: Vec<u8> = (0..len).map(|_| (rng.next_u64() & 0xFF) as u8).collect();
        let line = String::from_utf8_lossy(&bytes);
        let _ = Pop3Command::parse(&line);
    }
}

#[test]
fn parser_is_bounded_by_its_input() {
    // A very long line must not blow up or hang. `parse` borrows the line and
    // allocates at most an argument-sized String, so this is about catching a
    // future change that starts doing something quadratic or unbounded.
    for size in [1_024, 64 * 1_024, 1_024 * 1_024] {
        let long_argument = "A".repeat(size);

        let user = Pop3Command::parse(&format!("USER {long_argument}\r\n"));
        assert!(user.is_ok(), "a long USER argument is data, not an error");

        // A long numeric argument must be rejected as out of range, not
        // truncated into a valid-looking message number.
        let digits = "9".repeat(size.min(4_096));
        let retr = Pop3Command::parse(&format!("RETR {digits}\r\n"));
        assert!(
            retr.is_err(),
            "an unparseable message number must be an error"
        );
    }
}

#[test]
fn unknown_verbs_are_reported_without_echoing_arguments() {
    // `Unknown` carries the verb so the session can name it in `-ERR`. It must
    // not carry the rest of the line: that text is attacker-controlled and ends
    // up in a response and in logs.
    let parsed = Pop3Command::parse("EVILCMD rm -rf / --no-preserve-root\r\n")
        .expect("an unknown verb parses to Unknown");

    match parsed {
        Pop3Command::Unknown(verb) => {
            assert_eq!(verb, "EVILCMD");
            assert!(
                !verb.contains("rm -rf"),
                "the argument text must not travel with the verb"
            );
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn pass_never_reveals_its_argument_when_displayed() {
    // `Display` is what reaches the tracing output, so a password must not
    // survive it (ADR-035).
    let parsed = Pop3Command::parse("PASS hunter2 with spaces\r\n").expect("PASS parses");
    let rendered = parsed.to_string();

    assert!(
        !rendered.contains("hunter2"),
        "PASS must not render its argument: {rendered}"
    );
    assert_eq!(rendered, "PASS ********");
}
