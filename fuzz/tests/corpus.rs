//! Replays a fixed corpus through the fuzz checks on a stable toolchain.
//!
//! The libFuzzer binaries need nightly, so without this nothing would compile
//! the assertions in `src/lib.rs` in ordinary CI, and they could rot unnoticed
//! between fuzzing runs. This test costs a second and keeps them honest.
//!
//! It also replays crash artifacts: when fuzzing finds a panic, the reproducer
//! is committed under `fuzz/artifacts/<target>/` and from then on every commit
//! re-runs it. That is the regression loop `TESTING_STRATEGY.md` §9 asks for,
//! and it runs on stable rather than waiting for the nightly schedule.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("fuzz/ has a parent")
        .to_path_buf()
}

/// Every file under `dir`, recursively. Missing directories yield nothing: an
/// empty artifact directory is the normal state, not a failure.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The `.eml` corpus, which is the natural seed for the MIME target.
#[test]
fn every_fixture_survives_the_mime_checks() {
    let corpus = repo_root().join("testdata");
    let fixtures: Vec<_> = files_under(&corpus)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "eml"))
        .collect();

    assert!(
        fixtures.len() >= 100,
        "expected the full corpus, found {} fixtures under {}",
        fixtures.len(),
        corpus.display()
    );

    for fixture in fixtures {
        let bytes = fs::read(&fixture).expect("read fixture");
        sandesh_fuzz::check_mime(&bytes);
    }
}

/// Command lines are not `.eml` files, so the command targets get a small
/// hand-written corpus of the shapes that have historically broken parsers.
/// Fuzzing explores from here; this list is the floor.
const COMMAND_SEEDS: &[&[u8]] = &[
    b"",
    b" ",
    b"\0",
    b"A",
    b"a1 SELECT INBOX",
    b"a1 LOGIN user pass",
    b"a1 FETCH 1:* (BODY.PEEK[HEADER])",
    b"a1 APPEND INBOX {10}",
    b"a1 APPEND INBOX {999999999999}",
    b"a1 SEARCH (((((((((((TEXT x)))))))))))",
    b"a1 LIST \"\" \"*\"",
    b"a1 UID FETCH 4294967295 (FLAGS)",
    b"EHLO example.test",
    b"MAIL FROM:<>",
    b"MAIL FROM:<a@b> SIZE=99999999999999999999",
    b"RCPT TO:<<<<<<<>>>>>>>",
    b"DATA",
    b"VRFY \xff\xfe",
    b"USER x",
    b"PASS x",
    b"RETR 99999999999999999999",
    b"TOP 1 -1",
    b"UIDL",
    b"is:unread from:a@b subject:\"x y\"",
    b"before:not-a-date larger:99999g",
    b"((((((((((((((((a))))))))))))))))",
    // BDAT (RFC 3030), Phase 8 A4. The size is attacker-controlled and feeds a
    // counted read, so the overflow and nonsense cases are seeded explicitly
    // rather than left for the fuzzer to rediscover.
    b"BDAT 0 LAST",
    b"BDAT 99999999999999999999",
    b"BDAT -1",
    b"BDAT abc LAST",
    b"BDAT 5 SOON",
];

#[test]
fn command_seeds_survive_their_checks() {
    for seed in COMMAND_SEEDS {
        sandesh_fuzz::check_smtp(seed);
        sandesh_fuzz::check_imap(seed);
        sandesh_fuzz::check_pop3(seed);
        sandesh_fuzz::check_search(seed);
    }
}

/// Crash reproducers, once fuzzing has found any. Empty until it does.
#[test]
fn every_crash_artifact_stays_fixed() {
    let artifacts = Path::new(env!("CARGO_MANIFEST_DIR")).join("artifacts");
    for path in files_under(&artifacts) {
        let bytes = fs::read(&path).expect("read artifact");
        let target = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();

        match target.as_str() {
            "mime_parser" => sandesh_fuzz::check_mime(&bytes),
            "smtp_command" => sandesh_fuzz::check_smtp(&bytes),
            "imap_command" => sandesh_fuzz::check_imap(&bytes),
            "pop3_command" => sandesh_fuzz::check_pop3(&bytes),
            "search_query" => sandesh_fuzz::check_search(&bytes),
            other => panic!(
                "artifact {} sits under an unknown target directory {other:?}; \
                 artifacts belong in fuzz/artifacts/<target>/",
                path.display()
            ),
        }
    }
}
