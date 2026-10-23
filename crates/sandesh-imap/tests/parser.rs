//! IMAP command parser tests (RFC 9051 §9, phase brief §9 and §10).
//!
//! These are deliberately boundary-heavy. The parser is the only part of the
//! server that touches unauthenticated, attacker-controlled bytes, so its
//! failure modes matter more than its happy path: every malformed input below
//! must produce a `ParseError`, and none may panic or allocate on demand.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_imap::{
    Command, CommandKind, MAX_NESTING_DEPTH, ParseError, Parsed, Tag, Token, parse_line,
};

/// A literal limit used by tests that are not about the limit itself.
const LIMIT: usize = 64 * 1024;

/// Parses a command that is expected to complete on one line.
fn complete(line: &str) -> Command {
    match parse_line(line.as_bytes(), LIMIT) {
        Ok(Parsed::Complete(command)) => command,
        other => panic!("expected a complete command, got {other:?}"),
    }
}

/// The error a malformed line produces.
fn error(line: &str) -> ParseError {
    match parse_line(line.as_bytes(), LIMIT) {
        Err(err) => err,
        Ok(ok) => panic!("expected a parse error, got {ok:?}"),
    }
}

// ---------------------------------------------------------------- tags

#[test]
fn tag_and_command_are_parsed() {
    let command = complete("A001 CAPABILITY");
    assert_eq!(command.tag.as_str(), "A001");
    assert_eq!(command.kind, CommandKind::Capability);
    assert!(!command.uid_mode);
    assert!(command.args.is_empty());
}

#[test]
fn command_names_are_case_insensitive() {
    // RFC 9051 §9: commands are atoms, compared case-insensitively.
    for spelling in ["NOOP", "noop", "NoOp"] {
        assert_eq!(complete(&format!("t1 {spelling}")).kind, CommandKind::Noop);
    }
}

#[test]
fn a_plus_tag_is_rejected() {
    // `+` is reserved for continuation requests, so it cannot be a tag
    // (RFC 9051 §9 `tag`).
    assert_eq!(error("+ NOOP"), ParseError::InvalidTag);
}

#[test]
fn tags_containing_crlf_are_rejected() {
    // Echoing such a tag would let a client inject a whole forged response
    // line into the stream it is reading (response splitting).
    assert_eq!(error("A\r001 NOOP"), ParseError::InvalidTag);
    assert_eq!(error("A\n001 NOOP"), ParseError::InvalidTag);
}

#[test]
fn an_over_long_tag_is_rejected() {
    let long = "A".repeat(Tag::MAX_LEN + 1);
    assert_eq!(error(&format!("{long} NOOP")), ParseError::InvalidTag);
}

#[test]
fn empty_and_whitespace_lines_are_rejected() {
    assert_eq!(error(""), ParseError::Empty);
    assert_eq!(error("      "), ParseError::Empty);
}

#[test]
fn a_tag_with_no_command_is_rejected() {
    assert_eq!(error("A001"), ParseError::MissingCommand);
    assert_eq!(error("A001    "), ParseError::MissingCommand);
}

#[test]
fn unknown_commands_are_rejected() {
    assert_eq!(error("A001 FROBNICATE"), ParseError::UnknownCommand);
}

#[test]
fn lsub_is_rejected_because_rfc_9051_removed_it() {
    // Accepting LSUB would mean implementing IMAP4rev1 behaviour this server
    // does not have; RFC 9051 replaced it with LIST selection options.
    assert_eq!(error("A001 LSUB \"\" *"), ParseError::UnknownCommand);
}

#[test]
fn idle_parses_now_that_it_is_implemented() {
    // ADR-039 deferred IDLE and this test asserted it could not parse, so that
    // it could never be half-served. It is implemented now (RFC 2177), so the
    // assertion is inverted rather than deleted: the rule it protects — a
    // command parses exactly when the server can honour it — is unchanged.
    let command = complete("A001 IDLE");
    assert_eq!(command.kind, CommandKind::Idle);
    assert!(command.args.is_empty(), "IDLE takes no arguments");
}

#[test]
fn done_is_not_a_command() {
    // `DONE` is the untagged line that *ends* an IDLE (RFC 2177 §3), not a
    // command. It is read by the IDLE loop directly, and must not become a
    // command anyone can send: a tagged `DONE` outside IDLE means nothing.
    assert_eq!(error("A001 DONE"), ParseError::UnknownCommand);
}

// ---------------------------------------------------------------- UID prefix

#[test]
fn uid_prefix_sets_uid_mode_and_keeps_the_inner_command() {
    let command = complete("A001 UID FETCH 1:* (FLAGS)");
    assert_eq!(command.kind, CommandKind::Fetch);
    assert!(
        command.uid_mode,
        "UID FETCH is a FETCH over UIDs, not a distinct command (RFC 9051 §6.4.9)"
    );
}

#[test]
fn uid_is_refused_on_commands_that_do_not_take_it() {
    // RFC 9051 §6.4.9 lists the commands UID may prefix. SELECT is not one.
    assert_eq!(error("A001 UID SELECT INBOX"), ParseError::UidNotPermitted);
}

#[test]
fn uid_with_no_following_command_is_rejected() {
    assert_eq!(error("A001 UID"), ParseError::MissingCommand);
}

// ---------------------------------------------------------------- atoms, NIL

#[test]
fn nil_is_distinct_from_the_string_nil() {
    let command = complete("A001 LOGIN NIL \"NIL\"");
    assert_eq!(command.args[0], Token::Nil);
    assert_eq!(command.args[1], Token::Quoted(String::from("NIL")));
    assert_eq!(
        command.args[0].as_text(),
        None,
        "NIL must not flatten into the text \"NIL\": the difference is meaningful in IMAP"
    );
}

#[test]
fn nil_is_case_insensitive() {
    assert_eq!(complete("A001 LOGIN nil x").args[0], Token::Nil);
}

// ---------------------------------------------------------------- quoted strings

#[test]
fn quoted_strings_preserve_spaces() {
    let command = complete("A001 SELECT \"Sent Items\"");
    assert_eq!(command.args[0], Token::Quoted(String::from("Sent Items")));
}

#[test]
fn quoted_escapes_are_resolved() {
    // RFC 9051 §9 permits exactly two escapes inside a quoted string.
    let command = complete(r#"A001 SELECT "a\"b\\c""#);
    assert_eq!(command.args[0], Token::Quoted(String::from(r#"a"b\c"#)));
}

#[test]
fn an_unterminated_quoted_string_is_rejected() {
    assert_eq!(
        error("A001 SELECT \"unclosed"),
        ParseError::UnterminatedQuoted
    );
}

#[test]
fn a_trailing_backslash_cannot_run_off_the_end() {
    // The escape reader must not read past the buffer looking for its pair.
    assert_eq!(
        error(r#"A001 SELECT "abc\"#),
        ParseError::UnterminatedQuoted
    );
}

#[test]
fn an_invalid_escape_is_rejected() {
    assert_eq!(
        error(r#"A001 SELECT "a\nb""#),
        ParseError::IllegalQuotedChar
    );
}

#[test]
fn a_bare_newline_inside_a_quoted_string_is_rejected() {
    assert_eq!(error("A001 SELECT \"a\rb\""), ParseError::IllegalQuotedChar);
    assert_eq!(error("A001 SELECT \"a\nb\""), ParseError::IllegalQuotedChar);
}

#[test]
fn an_empty_quoted_string_is_valid() {
    // `LIST "" *` is the canonical mailbox listing, so this must work.
    let command = complete("A001 LIST \"\" *");
    assert_eq!(command.args[0], Token::Quoted(String::new()));
    assert_eq!(command.args[1], Token::Atom(String::from("*")));
}

// ---------------------------------------------------------------- lists

#[test]
fn parenthesised_lists_are_parsed() {
    let command = complete("A001 STORE 1 +FLAGS (\\Seen \\Deleted)");
    assert_eq!(
        command.args[2],
        Token::List(vec![
            Token::Atom(String::from("\\Seen")),
            Token::Atom(String::from("\\Deleted")),
        ])
    );
}

#[test]
fn an_empty_list_is_valid() {
    assert_eq!(
        complete("A001 STORE 1 FLAGS ()").args[2],
        Token::List(vec![])
    );
}

#[test]
fn lists_nest() {
    let command = complete("A001 FETCH 1 (BODY (HEADER TEXT))");
    assert_eq!(
        command.args[1],
        Token::List(vec![
            Token::Atom(String::from("BODY")),
            Token::List(vec![
                Token::Atom(String::from("HEADER")),
                Token::Atom(String::from("TEXT")),
            ]),
        ])
    );
}

#[test]
fn an_unterminated_list_is_rejected() {
    assert_eq!(error("A001 FETCH 1 (FLAGS"), ParseError::UnterminatedList);
}

#[test]
fn an_unbalanced_closing_paren_is_rejected() {
    assert_eq!(error("A001 FETCH 1 FLAGS)"), ParseError::UnbalancedParen);
}

#[test]
fn deeply_nested_lists_are_bounded_rather_than_overflowing_the_stack() {
    // Without a depth limit this input recurses until the stack is exhausted,
    // which is a remote crash. The limit turns it into a `BAD` response.
    let bomb = format!("A001 FETCH 1 {}", "(".repeat(MAX_NESTING_DEPTH + 5));
    assert_eq!(error(&bomb), ParseError::NestingTooDeep);

    // A pathological input far beyond the limit must still be a clean error.
    let huge = format!("A001 FETCH 1 {}", "(".repeat(100_000));
    assert_eq!(error(&huge), ParseError::NestingTooDeep);
}

#[test]
fn nesting_up_to_the_limit_is_accepted() {
    // The limit must not be off by one against legitimate input.
    let line = format!(
        "A001 FETCH 1 {}x{}",
        "(".repeat(MAX_NESTING_DEPTH),
        ")".repeat(MAX_NESTING_DEPTH)
    );
    let command = complete(&line);
    assert_eq!(command.kind, CommandKind::Fetch);
}

#[test]
fn too_many_tokens_are_rejected() {
    let many = "x ".repeat(4096);
    assert_eq!(
        error(&format!("A001 FETCH {many}")),
        ParseError::TooManyTokens
    );
}

// ---------------------------------------------------------------- literals

#[test]
fn a_literal_pauses_parsing_and_reports_its_length() {
    let pending = match parse_line(b"A001 LOGIN {5}", LIMIT) {
        Ok(Parsed::NeedLiteral(pending)) => pending,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    assert_eq!(pending.length(), 5);
    assert!(
        !pending.is_non_synchronising(),
        "a plain {{n}} literal requires a continuation request"
    );
}

#[test]
fn a_literal_payload_completes_the_command() {
    let pending = match parse_line(b"A001 LOGIN {5}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    let parsed = pending
        .resume(b"alice".to_vec(), b" secret", LIMIT)
        .expect("resume should succeed");
    let Parsed::Complete(command) = parsed else {
        panic!("expected the command to complete");
    };
    assert_eq!(command.kind, CommandKind::Login);
    assert_eq!(command.args[0], Token::Literal(b"alice".to_vec()));
    assert_eq!(command.args[1], Token::Atom(String::from("secret")));
}

#[test]
fn a_non_synchronising_literal_is_recognised() {
    // RFC 7888 LITERAL+: the client is already sending the octets, so the
    // server must NOT answer with a continuation request. Getting this wrong
    // desynchronises the connection.
    let pending = match parse_line(b"A001 LOGIN {5+}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    assert!(pending.is_non_synchronising());
    assert_eq!(pending.length(), 5);
}

#[test]
fn a_zero_length_literal_is_valid() {
    let pending = match parse_line(b"A001 LOGIN {0}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    assert_eq!(pending.length(), 0);
    let parsed = pending
        .resume(Vec::new(), b"", LIMIT)
        .expect("an empty literal is a valid argument");
    let Parsed::Complete(command) = parsed else {
        panic!("expected completion");
    };
    assert_eq!(command.args[0], Token::Literal(Vec::new()));
}

#[test]
fn a_literal_at_exactly_the_limit_is_accepted() {
    // Boundary: the limit is inclusive, so `{N}` with N == limit must pass.
    let pending = match parse_line(format!("A001 LOGIN {{{LIMIT}}}").as_bytes(), LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral at the limit, got {other:?}"),
    };
    assert_eq!(pending.length(), LIMIT);
}

#[test]
fn an_oversized_literal_is_rejected_before_anything_is_allocated() {
    // The whole point: `{4294967295}` must cost a rejection, not 4 GiB.
    let err = error("A001 LOGIN {4294967295}");
    assert_eq!(err, ParseError::LiteralTooLarge { limit: LIMIT });

    // One octet over the limit is also refused — no off-by-one slack.
    assert_eq!(
        error(&format!("A001 LOGIN {{{}}}", LIMIT + 1)),
        ParseError::LiteralTooLarge { limit: LIMIT }
    );
}

#[test]
fn a_literal_length_that_cannot_fit_a_usize_is_malformed_not_an_overflow() {
    // Twenty digits overflows u64. It must be reported as malformed rather
    // than wrapping to a small, permitted length.
    assert_eq!(
        error("A001 LOGIN {99999999999999999999999}"),
        ParseError::MalformedLiteral
    );
}

#[test]
fn malformed_literal_declarations_are_rejected() {
    for bad in [
        "A001 LOGIN {",        // no digits, no close
        "A001 LOGIN {}",       // no digits
        "A001 LOGIN {12",      // unterminated
        "A001 LOGIN {abc}",    // not a number
        "A001 LOGIN {-1}",     // negative
        "A001 LOGIN {5}extra", // junk after the brace: CRLF must follow it
        "A001 LOGIN {5 }",     // space inside
    ] {
        assert_eq!(
            error(bad),
            ParseError::MalformedLiteral,
            "expected {bad:?} to be a malformed literal"
        );
    }
}

#[test]
fn multiple_literals_in_one_command_are_supported() {
    // `LOGIN {5}\r\nalice {6}\r\nsecret` — two literals, one command.
    let first = match parse_line(b"A001 LOGIN {5}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected the first literal, got {other:?}"),
    };
    let second = match first.resume(b"alice".to_vec(), b" {6}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected a second literal, got {other:?}"),
    };
    assert_eq!(second.length(), 6);

    let Parsed::Complete(command) = second
        .resume(b"secret".to_vec(), b"", LIMIT)
        .expect("the command should complete after the second literal")
    else {
        panic!("expected completion");
    };
    assert_eq!(command.args[0], Token::Literal(b"alice".to_vec()));
    assert_eq!(command.args[1], Token::Literal(b"secret".to_vec()));
}

#[test]
fn literal_payloads_preserve_arbitrary_bytes() {
    // A literal is explicitly binary (RFC 9051 §4.3). NUL, CR, LF, invalid
    // UTF-8 and high bytes must all survive byte for byte — the raw-fidelity
    // guarantee the product rests on.
    let payload = vec![0x00, 0x0d, 0x0a, 0xff, 0xfe, 0x80, b'a'];
    let pending = match parse_line(
        format!("A001 APPEND INBOX {{{}}}", payload.len()).as_bytes(),
        LIMIT,
    ) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    let Parsed::Complete(command) = pending
        .resume(payload.clone(), b"", LIMIT)
        .expect("binary payloads are valid")
    else {
        panic!("expected completion");
    };
    assert_eq!(
        command.args[1],
        Token::Literal(payload),
        "literal octets must not be transcoded or truncated"
    );
}

#[test]
fn a_literal_inside_a_list_stays_inside_that_list() {
    // The pending-literal state must remember it was inside an open list;
    // otherwise the resumed token lands at the wrong nesting level.
    let pending = match parse_line(b"A001 SEARCH (SUBJECT {3}", LIMIT) {
        Ok(Parsed::NeedLiteral(p)) => p,
        other => panic!("expected NeedLiteral, got {other:?}"),
    };
    let Parsed::Complete(command) = pending
        .resume(b"hey".to_vec(), b")", LIMIT)
        .expect("the list should close after the literal")
    else {
        panic!("expected completion");
    };
    assert_eq!(
        command.args[0],
        Token::List(vec![
            Token::Atom(String::from("SUBJECT")),
            Token::Literal(b"hey".to_vec()),
        ])
    );
}

// ---------------------------------------------------------------- robustness

#[test]
fn no_input_can_panic_the_parser() {
    // A parser panic is a remote denial of service. This sweeps a range of
    // hostile and simply strange inputs and asserts only that each one
    // returns rather than unwinding.
    let inputs: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\0".to_vec(),
        b"\r\n".to_vec(),
        b"{".to_vec(),
        b"}".to_vec(),
        b"\"".to_vec(),
        b"(".to_vec(),
        b")".to_vec(),
        b"A001".to_vec(),
        b"A001 ".to_vec(),
        b"A001 LOGIN \\".to_vec(),
        b"A001 LOGIN {5}{5}".to_vec(),
        b"UID".to_vec(),
        b"A001 UID UID FETCH 1".to_vec(),
        vec![0xff; 256],
        vec![b'('; 1000],
        vec![b'"'; 1000],
        vec![b'{'; 1000],
        b"A001 FETCH \xff\xfe".to_vec(),
    ];
    for input in inputs {
        // The result is irrelevant; not unwinding is the assertion.
        let _ = parse_line(&input, LIMIT);
    }
}

#[test]
fn extra_whitespace_between_arguments_is_tolerated() {
    let command = complete("A001    SELECT     INBOX");
    assert_eq!(command.kind, CommandKind::Select);
    assert_eq!(command.args[0], Token::Atom(String::from("INBOX")));
}
