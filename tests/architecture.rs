//! Mechanical enforcement of the dependency direction.
//!
//! `docs/ARCHITECTURE.md` §12 states invariants that decay within months unless
//! a machine checks them. This test is that machine.
//!
//! # How it works
//!
//! It runs `cargo metadata`, which reports the **resolved** dependency graph —
//! every crate that actually ends up compiled into a target, including
//! transitive ones. It then walks that graph from each Sandesh crate and
//! asserts what may and may not be reachable.
//!
//! This is deliberately not a filename or `grep` check. A textual search would
//! miss a banned crate arriving transitively (the likeliest way an invariant
//! breaks in practice) and would produce false alarms on comments and strings.
//! Walking the resolved graph catches a violation however it arrives.
//!
//! Dev-dependencies are excluded from the core's ban list: a test may legitimately
//! use `tokio` to drive an async port without that runtime becoming part of the
//! library's dependency tree.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// One package in the resolved graph.
struct Package {
    name: String,
    /// Normal (non-dev, non-build) dependency package ids.
    runtime_deps: Vec<String>,
    /// Every dependency, including dev and build.
    all_deps: Vec<String>,
}

/// The resolved workspace graph.
struct Graph {
    packages: BTreeMap<String, Package>,
    by_name: BTreeMap<String, String>,
}

impl Graph {
    fn load() -> Self {
        let output = Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--format-version",
                "1",
                "--all-features",
                "--manifest-path",
            ])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .output()
            .expect("run cargo metadata");

        assert!(
            output.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let json = String::from_utf8(output.stdout).expect("metadata is utf-8");
        parse_metadata(&json)
    }

    /// Every package reachable from `root` through runtime dependencies.
    fn runtime_closure(&self, root: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let Some(start) = self.by_name.get(root) else {
            panic!("package {root} is not in the workspace");
        };
        let mut stack = vec![start.clone()];

        while let Some(id) = stack.pop() {
            let Some(package) = self.packages.get(&id) else {
                continue;
            };
            if !seen.insert(package.name.clone()) {
                continue;
            }
            for dep in &package.runtime_deps {
                stack.push(dep.clone());
            }
        }
        seen.remove(root);
        seen
    }

    /// Direct dependencies of `root`, including dev-dependencies.
    /// A package's direct **runtime** dependencies, excluding dev and build.
    ///
    /// The distinction matters for rules about what production code may reach:
    /// a test client legitimately needs `rustls` to speak TLS to the server it
    /// is testing, while the library itself must not configure TLS at all.
    /// Using `direct_deps` for such a rule would conflate the two and force the
    /// tests to be worse to keep the architecture check passing.
    fn direct_runtime_deps(&self, root: &str) -> BTreeSet<String> {
        let Some(id) = self.by_name.get(root) else {
            panic!("package {root} is not in the workspace");
        };
        self.packages
            .get(id)
            .map(|p| {
                p.runtime_deps
                    .iter()
                    .filter_map(|d| self.packages.get(d).map(|p| p.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every `sandesh-*` package in the workspace, by name.
    ///
    /// Enumerated from the resolved graph so a rule stated over "every crate"
    /// covers crates added after the rule was written — the failure mode a
    /// hand-written list has, and which let `sandesh-desktop` in unnoticed.
    fn sandesh_packages(&self) -> BTreeSet<String> {
        self.by_name
            .keys()
            .filter(|name| name.starts_with("sandesh-"))
            .cloned()
            .collect()
    }

    fn direct_deps(&self, root: &str) -> BTreeSet<String> {
        let Some(id) = self.by_name.get(root) else {
            panic!("package {root} is not in the workspace");
        };
        self.packages
            .get(id)
            .map(|p| {
                p.all_deps
                    .iter()
                    .filter_map(|d| self.packages.get(d).map(|p| p.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Minimal JSON extraction for the fields we need.
///
/// A hand-rolled reader avoids adding `serde_json` as a dependency of the
/// architecture test itself — a test that guards dependency hygiene should not
/// import a dependency to do it.
fn parse_metadata(json: &str) -> Graph {
    let mut packages = BTreeMap::new();
    let mut by_name = BTreeMap::new();

    // "resolve" -> "nodes": [{ "id", "deps": [{ "pkg", "dep_kinds": [{"kind"}] }] }]
    let resolve = json
        .find("\"resolve\":")
        .map(|i| &json[i..])
        .expect("metadata has a resolve section");

    for node in resolve.split("{\"id\":\"").skip(1) {
        let Some(id_end) = node.find('"') else {
            continue;
        };
        let id = &node[..id_end];
        let name = package_name(id);

        let deps_section = node.find("\"deps\":[").map(|i| &node[i..]).unwrap_or("");
        let mut runtime_deps = Vec::new();
        let mut all_deps = Vec::new();

        for dep in deps_section.split("{\"name\":").skip(1) {
            let Some(pkg_start) = dep.find("\"pkg\":\"") else {
                continue;
            };
            let rest = &dep[pkg_start + 7..];
            let Some(pkg_end) = rest.find('"') else {
                continue;
            };
            let pkg_id = rest[..pkg_end].to_owned();

            // dep_kinds: absent "kind" means a normal dependency; "dev"/"build"
            // are labelled explicitly.
            let kinds_end = dep.find("}],").unwrap_or(dep.len());
            let kinds = &dep[..kinds_end];
            let is_dev_or_build =
                kinds.contains("\"kind\":\"dev\"") || kinds.contains("\"kind\":\"build\"");
            let has_normal = kinds.contains("\"kind\":null") || !is_dev_or_build;

            if has_normal {
                runtime_deps.push(pkg_id.clone());
            }
            all_deps.push(pkg_id);
        }

        by_name.insert(name.clone(), id.to_owned());
        packages.insert(
            id.to_owned(),
            Package {
                name,
                runtime_deps,
                all_deps,
            },
        );
    }

    assert!(
        !packages.is_empty(),
        "failed to parse any packages from cargo metadata"
    );
    Graph { packages, by_name }
}

/// Extracts the package name from a package id.
///
/// Cargo emits two shapes, and both occur in one graph:
///
/// * registry packages — `registry+<url>#<name>@<version>`
/// * path packages — `path+file:///<dir>/<name>#<version>`
///
/// For the path form the name is the final directory component, not the text
/// after `#` (which is the bare version).
fn package_name(id: &str) -> String {
    let Some(hash) = id.rfind('#') else {
        return id.to_owned();
    };
    let (before, after) = (&id[..hash], &id[hash + 1..]);

    // `#name@version` — the registry form.
    if let Some((name, _version)) = after.rsplit_once('@') {
        return name.to_owned();
    }
    // `#version` alone: a version starts with a digit, so anything else is a
    // bare name and can be taken directly.
    if !after.starts_with(|c: char| c.is_ascii_digit()) {
        return after.to_owned();
    }
    // Path form: the name is the last path segment before the '#'.
    before
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(before)
        .to_owned()
}

/// Crates that must never reach `sandesh-core` at runtime.
///
/// Each entry corresponds to a specific architectural rule rather than a
/// general dislike of the crate.
const BANNED_FROM_CORE: &[(&str, &str)] = &[
    ("sqlx", "core must not know about the database"),
    ("sqlx-core", "core must not know about the database"),
    ("sqlx-sqlite", "core must not know about the database"),
    ("libsqlite3-sys", "core must not link SQLite"),
    ("rusqlite", "core must not know about the database"),
    ("axum", "core must not know about HTTP"),
    ("axum-core", "core must not know about HTTP"),
    ("hyper", "core must not know about HTTP"),
    ("tower", "core must not know about HTTP"),
    ("tower-http", "core must not know about HTTP"),
    ("tokio", "core must be runtime-independent"),
    ("tokio-util", "core must be runtime-independent"),
    ("async-std", "core must be runtime-independent"),
    ("smol", "core must be runtime-independent"),
    ("tungstenite", "core must not know about WebSocket"),
    ("tokio-tungstenite", "core must not know about WebSocket"),
    ("rustls", "core must not know about TLS"),
    ("lettre", "core must not embed a mail client"),
    ("imap-codec", "protocol codecs belong in adapter crates"),
    ("imap-types", "protocol codecs belong in adapter crates"),
    // ADR-085: outbound HTTP belongs to `sandesh-webhook`. Without this, A2
    // could satisfy its ADR by accident today and drift tomorrow.
    (
        "hyper-util",
        "core must not dial out; that is sandesh-webhook's job",
    ),
    ("http-body-util", "core must not know about HTTP bodies"),
];

#[test]
fn core_has_no_infrastructure_dependencies() {
    // ARCHITECTURE §12.1 and §12.9.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-core");

    let mut violations = Vec::new();
    for (crate_name, reason) in BANNED_FROM_CORE {
        if closure.contains(*crate_name) {
            violations.push(format!("  {crate_name}: {reason}"));
        }
    }

    assert!(
        violations.is_empty(),
        "sandesh-core has forbidden dependencies:\n{}\n\nfull closure: {closure:?}",
        violations.join("\n")
    );
}

#[test]
fn core_depends_on_no_sandesh_crate() {
    // The core is the innermost layer: nothing in the project may sit beneath it.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-core");

    let sandesh_deps: Vec<_> = closure
        .iter()
        .filter(|name| name.starts_with("sandesh-"))
        .collect();

    assert!(
        sandesh_deps.is_empty(),
        "sandesh-core must not depend on any other workspace crate, found {sandesh_deps:?}"
    );
}

#[test]
fn storage_depends_inward_on_core_only() {
    // ARCHITECTURE §12.2: storage may see the core and nothing else in the project.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-storage");

    assert!(
        closure.contains("sandesh-core"),
        "storage must implement the core's ports"
    );

    let others: Vec<_> = closure
        .iter()
        .filter(|name| name.starts_with("sandesh-") && *name != "sandesh-core")
        .collect();

    assert!(
        others.is_empty(),
        "sandesh-storage must depend only on sandesh-core, found {others:?}"
    );
}

#[test]
fn webhook_depends_inward_on_core_only() {
    // ADR-085. The same rule `sandesh-storage` is held to: an adapter crate
    // sees the core and nothing else in the project. Asserted rather than
    // assumed, because the whole reason A2 got its own crate was so this
    // invariant could be stated somewhere a test can fail.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-webhook");

    assert!(
        closure.contains("sandesh-core"),
        "sandesh-webhook must implement the core's OutboxStore-facing work"
    );

    let others: Vec<_> = closure
        .iter()
        .filter(|name| name.starts_with("sandesh-") && *name != "sandesh-core")
        .collect();

    assert!(
        others.is_empty(),
        "sandesh-webhook must depend only on sandesh-core, found {others:?}"
    );
}

#[test]
fn webhook_owns_the_outbound_http_client() {
    // The positive half of ADR-085. The ban list above says core must not
    // reach the client; this says the crate that was created to own it
    // actually does, so the ban cannot be satisfied by nobody having one.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-webhook");

    assert!(
        closure.contains("hyper-util"),
        "sandesh-webhook owns outbound HTTP (ADR-085); found closure {closure:?}"
    );
}

#[test]
fn core_declares_only_its_permitted_direct_dependencies() {
    // A tighter check than the ban list: it catches an *addition* nobody thought
    // to ban. ARCHITECTURE §3.2 fixes this list; changing it is an architectural
    // decision that belongs in DECISIONS.md.
    let graph = Graph::load();
    let permitted: BTreeSet<&str> = [
        "mail-parser",
        "thiserror",
        "uuid",
        "time",
        "sha2",
        "async-trait",
        // ADR-102: HMAC signing is a pure function over bytes, with no I/O and
        // no runtime, so it sits beside the retry policy rather than in the
        // crate that owns the socket. `hex` renders the digest, `subtle`
        // compares it in constant time.
        "hmac",
        "hex",
        "subtle",
    ]
    .into_iter()
    .collect();

    let declared = graph.direct_deps("sandesh-core");
    let unexpected: Vec<_> = declared
        .iter()
        .filter(|name| !permitted.contains(name.as_str()))
        .collect();

    assert!(
        unexpected.is_empty(),
        "sandesh-core gained direct dependencies not listed in ARCHITECTURE §3.2: {unexpected:?}\n\
         If this is intentional, update the architecture document and this test together."
    );
}

#[test]
fn smtp_depends_on_core_proto_and_config_only() {
    // ARCHITECTURE §3.2: a protocol adapter may see the domain, the shared
    // transport scaffolding, and configuration — nothing else in the project.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-smtp");

    for required in ["sandesh-core", "sandesh-proto", "sandesh-config"] {
        assert!(
            closure.contains(required),
            "sandesh-smtp should depend on {required}"
        );
    }

    let forbidden: Vec<_> = closure
        .iter()
        .filter(|name| {
            name.starts_with("sandesh-")
                && !matches!(
                    name.as_str(),
                    "sandesh-core" | "sandesh-proto" | "sandesh-config"
                )
        })
        .collect();

    assert!(
        forbidden.is_empty(),
        "sandesh-smtp must not depend on {forbidden:?}; \
         it reaches storage only through the core's ports"
    );
}

#[test]
fn smtp_has_no_database_dependency() {
    // The rule that keeps SQL out of the protocol layer. An adapter that could
    // see sqlx would eventually use it.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-smtp");

    for banned in [
        "sqlx",
        "sqlx-core",
        "sqlx-sqlite",
        "libsqlite3-sys",
        "rusqlite",
    ] {
        assert!(
            !closure.contains(banned),
            "sandesh-smtp must not reach {banned}: protocol adapters issue no SQL"
        );
    }
}

#[test]
fn imap_depends_on_core_proto_and_config_only() {
    // ARCHITECTURE §3.2, phase brief §26. The IMAP adapter is bound by exactly
    // the rule SMTP is: the domain, the shared transport scaffolding, and
    // configuration. In particular it must not reach `sandesh-storage`, because
    // its only route to stored mail is the ports `sandesh-core` defines.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-imap");

    for required in ["sandesh-core", "sandesh-proto", "sandesh-config"] {
        assert!(
            closure.contains(required),
            "sandesh-imap should depend on {required}"
        );
    }

    let forbidden: Vec<_> = closure
        .iter()
        .filter(|name| {
            name.starts_with("sandesh-")
                && !matches!(
                    name.as_str(),
                    "sandesh-core" | "sandesh-proto" | "sandesh-config"
                )
        })
        .collect();

    assert!(
        forbidden.is_empty(),
        "sandesh-imap must not depend on {forbidden:?}; \
         it reaches storage only through the core's ports"
    );
}

#[test]
fn imap_reaches_storage_only_through_the_core_ports() {
    // Phase 5 increment 2. The IMAP session authenticates by reading an account
    // through `AccountStore`, a trait `sandesh-core` defines. It must NOT gain a
    // dependency on `sandesh-storage`, which would let it issue a query and
    // bypass the port boundary the whole architecture rests on.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-imap");

    assert!(
        !closure.contains("sandesh-storage"),
        "sandesh-imap must reach storage only through the core's ports; \
         a direct dependency on sandesh-storage defeats that"
    );
    assert!(
        !closure.contains("sandesh-smtp"),
        "sandesh-imap must not depend on sandesh-smtp: the two adapters share \
         code through sandesh-proto, not through each other"
    );
}

#[test]
fn imap_does_not_duplicate_the_mime_parser() {
    // Phase brief §11 and §27. `FETCH BODYSTRUCTURE` and `FETCH BODY[<part>]`
    // need a MIME tree and per-part byte ranges, and the temptation is to parse
    // the message again inside the protocol adapter. The core already exposes
    // both (`ParsedMessage::parts`, `MimePart::body`/`headers`), and a second
    // parser would drift from it — two answers to "what is part 2" is worse
    // than a missing feature.
    //
    // `mail-parser` is therefore permitted to `sandesh-core` alone. It still
    // appears in the IMAP crate's *transitive* closure through core, which is
    // correct; what must not happen is a direct dependency, because that is the
    // only way the adapter could call it itself.
    let graph = Graph::load();
    let direct = graph.direct_runtime_deps("sandesh-imap");

    assert!(
        !direct.contains("mail-parser"),
        "sandesh-imap must not depend on mail-parser directly: MIME structure \
         comes from the core model, not from a second parser in the adapter"
    );
    assert!(
        graph
            .runtime_closure("sandesh-imap")
            .contains("sandesh-core"),
        "the core is how the adapter reaches MIME structure"
    );
}

#[test]
fn imap_does_not_duplicate_tls_configuration() {
    // Phase 5 increment 2, §14. The IMAP crate accepts a `TlsAcceptor` that the
    // composition root builds from `sandesh_proto::Identity`; it must not load
    // certificates or build a rustls server configuration itself. `rcgen` and
    // `rustls-pemfile` are how that would show up, so their absence from the
    // direct dependencies is the mechanical check.
    let graph = Graph::load();
    let direct = graph.direct_runtime_deps("sandesh-imap");

    for banned in ["rcgen", "rustls-pemfile", "rustls"] {
        assert!(
            !direct.contains(banned),
            "sandesh-imap must not depend on {banned} directly: certificate \
             loading and rustls configuration belong to sandesh-proto \
             (ADR-034), and a second copy is how the two would diverge"
        );
    }

    // `tokio-rustls` *is* permitted: the crate names `TlsAcceptor` in its
    // signature to receive one. That is using the acceptor, not configuring it.
    assert!(
        direct.contains("tokio-rustls"),
        "sandesh-imap is expected to name tokio-rustls to accept an injected \
         acceptor; if this changed, update this test deliberately"
    );
}

#[test]
fn imap_has_no_database_dependency() {
    // The same rule that keeps SQL out of SMTP. IMAP is the protocol most
    // tempted to reach for a query — FETCH and SEARCH are database-shaped — so
    // the boundary is asserted rather than trusted.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-imap");

    for banned in [
        "sqlx",
        "sqlx-core",
        "sqlx-sqlite",
        "libsqlite3-sys",
        "rusqlite",
    ] {
        assert!(
            !closure.contains(banned),
            "sandesh-imap must not reach {banned}: protocol adapters issue no SQL"
        );
    }
}

#[test]
fn imap_implements_its_own_codec_and_reuses_phase_45_security() {
    // Two rules in one place because they are the same principle: the IMAP
    // adapter writes the protocol itself (ADR-002 — no imap-codec/imap-types,
    // which the core dependency test also bans) but must NOT write its own
    // security primitives. Argon2, base64 and rustls reach it only through
    // `sandesh-proto`, so a second password verifier or TLS config cannot
    // appear here (phase brief §3, §7, §26).
    let graph = Graph::load();

    // Runtime dependencies only: the integration tests are *allowed* to use a
    // real TLS client and a base64 encoder to drive the server, and forbidding
    // that would mean testing less rather than depending less.
    let direct = graph.direct_runtime_deps("sandesh-imap");
    for banned in ["imap-codec", "imap-types", "imap-proto", "async-imap"] {
        assert!(
            !direct.contains(banned),
            "sandesh-imap must implement RFC 9051 itself, not via {banned}"
        );
    }

    // Cryptography must not be a direct runtime dependency of the IMAP crate:
    // that is what would let it grow a duplicate credential verifier or a
    // second TLS configuration. `argon2`, `base64` and `subtle` reach it only
    // through `sandesh-proto`.
    //
    // `tokio-rustls` is deliberately absent from this list. The server names
    // `TlsAcceptor` in `with_tls` to *receive* an acceptor the composition root
    // built from `sandesh_proto::Identity` — it uses the acceptor without
    // configuring it, which is the same arrangement `sandesh-smtp` has.
    // Configuring TLS is what is forbidden, and
    // `imap_does_not_duplicate_tls_configuration` checks that separately by
    // banning `rustls`, `rcgen` and `rustls-pemfile`.
    for reused in ["argon2", "base64", "subtle"] {
        assert!(
            !direct.contains(reused),
            "sandesh-imap must reuse {reused} through sandesh-proto rather than \
             depending on it directly; a second security implementation is the \
             thing Phase 4.5 exists to prevent"
        );
    }
}

#[test]
fn pop3_depends_on_core_proto_and_config_only() {
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-pop3");

    for required in ["sandesh-core", "sandesh-proto", "sandesh-config"] {
        assert!(
            closure.contains(required),
            "sandesh-pop3 should depend on {required}"
        );
    }

    let forbidden: Vec<_> = closure
        .iter()
        .filter(|name| {
            name.starts_with("sandesh-")
                && !matches!(
                    name.as_str(),
                    "sandesh-core" | "sandesh-proto" | "sandesh-config"
                )
        })
        .collect();

    assert!(
        forbidden.is_empty(),
        "sandesh-pop3 must not depend on {forbidden:?}; \
         it reaches storage only through the core's ports"
    );
}

#[test]
fn pop3_reaches_storage_only_through_the_core_ports() {
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-pop3");

    assert!(
        !closure.contains("sandesh-storage"),
        "sandesh-pop3 must reach storage only through the core's ports; \
         a direct dependency on sandesh-storage defeats that"
    );
    assert!(
        !closure.contains("sandesh-smtp"),
        "sandesh-pop3 must not depend on sandesh-smtp"
    );
    assert!(
        !closure.contains("sandesh-imap"),
        "sandesh-pop3 must not depend on sandesh-imap"
    );
}

#[test]
fn pop3_does_not_duplicate_tls_configuration() {
    let graph = Graph::load();
    let direct = graph.direct_runtime_deps("sandesh-pop3");

    for banned in ["rcgen", "rustls-pemfile", "rustls"] {
        assert!(
            !direct.contains(banned),
            "sandesh-pop3 must not depend on {banned} directly: certificate \
             loading and rustls configuration belong to sandesh-proto"
        );
    }

    assert!(
        direct.contains("tokio-rustls"),
        "sandesh-pop3 is expected to name tokio-rustls to accept an injected \
         acceptor"
    );
}

#[test]
fn pop3_has_no_database_dependency() {
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-pop3");

    for banned in [
        "sqlx",
        "sqlx-core",
        "sqlx-sqlite",
        "libsqlite3-sys",
        "rusqlite",
    ] {
        assert!(
            !closure.contains(banned),
            "sandesh-pop3 must not reach {banned}: protocol adapters issue no SQL"
        );
    }
}

#[test]
fn pop3_implements_its_own_codec_and_reuses_phase_45_security() {
    let graph = Graph::load();
    let direct = graph.direct_runtime_deps("sandesh-pop3");

    for banned in ["pop3-codec", "async-pop3"] {
        assert!(
            !direct.contains(banned),
            "sandesh-pop3 must implement RFC 1939 itself, not via {banned}"
        );
    }

    for reused in ["argon2", "base64", "subtle"] {
        assert!(
            !direct.contains(reused),
            "sandesh-pop3 must reuse {reused} through sandesh-proto rather than \
             depending on it directly"
        );
    }
}

#[test]
fn proto_stays_protocol_agnostic() {
    // sandesh-proto exists to be shared by SMTP, IMAP and POP3. Depending on
    // any protocol crate would defeat that, and depending on the database would
    // put storage below the transport layer.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-proto");

    let forbidden: Vec<_> = closure
        .iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "sandesh-smtp"
                    | "sandesh-imap"
                    | "sandesh-pop3"
                    | "sandesh-storage"
                    | "sqlx"
                    | "axum"
            )
        })
        .collect();

    assert!(
        forbidden.is_empty(),
        "sandesh-proto must stay reusable by every protocol; found {forbidden:?}"
    );
}

#[test]
fn config_is_dependency_free() {
    // Configuration is read by every layer, so anything heavy here propagates
    // everywhere — including into the core's neighbours.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-config");

    for banned in ["tokio", "sqlx", "axum", "sandesh-core", "sandesh-storage"] {
        assert!(
            !closure.contains(banned),
            "sandesh-config must stay light; found {banned}"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 3: events, API, and the composition root
// ---------------------------------------------------------------------------

#[test]
fn events_depends_inward_on_core_only() {
    // ARCHITECTURE §3.2: the bus implements a core port and knows nothing else
    // in the project. If it grew a storage or protocol dependency it would stop
    // being a neutral transport and start being a second place mail logic lives.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-events");

    assert!(
        closure.contains("sandesh-core"),
        "the bus must implement the core's EventSink port"
    );

    let others: Vec<_> = closure
        .iter()
        .filter(|name| name.starts_with("sandesh-") && *name != "sandesh-core")
        .collect();
    assert!(
        others.is_empty(),
        "sandesh-events must depend only on sandesh-core, found {others:?}"
    );
}

#[test]
fn the_event_bus_knows_nothing_about_any_transport() {
    // EVENT_MODEL §1.4 and §7: translation to WebSocket happens in the API, not
    // in the bus. The same events drive IMAP IDLE, and a bus that imported a
    // WebSocket type would quietly become WebSocket-shaped.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-events");

    for (banned, reason) in [
        ("axum", "the bus must not know about HTTP"),
        ("axum-core", "the bus must not know about HTTP"),
        ("hyper", "the bus must not know about HTTP"),
        ("tower-http", "the bus must not know about HTTP"),
        ("tungstenite", "the bus must not know about WebSocket"),
        ("tokio-tungstenite", "the bus must not know about WebSocket"),
        ("sqlx", "the bus must not know about the database"),
        (
            "serde_json",
            "the wire format belongs to the API, not the bus",
        ),
    ] {
        assert!(
            !closure.contains(banned),
            "sandesh-events depends on {banned}: {reason}"
        );
    }
}

#[test]
fn the_api_never_learns_how_storage_works() {
    // ARCHITECTURE §3.2. The API reaches storage only through core ports, so a
    // handler cannot write SQL even by accident — this is what makes "no
    // business logic in HTTP" structurally true rather than a review rule.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-api");

    for banned in [
        "sqlx",
        "sqlx-core",
        "sqlx-sqlite",
        "libsqlite3-sys",
        "rusqlite",
    ] {
        assert!(
            !closure.contains(banned),
            "sandesh-api depends on {banned}: the API must reach storage only \
             through sandesh-core's ports"
        );
    }
    assert!(
        !closure.contains("sandesh-storage"),
        "sandesh-api must not depend on the storage implementation"
    );
}

#[test]
fn the_api_depends_on_no_protocol_crate() {
    // §59: SMTP must not know REST exists, and REST must not know SMTP exists.
    // They are siblings, and siblings do not see each other.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-api");

    for protocol in [
        "sandesh-smtp",
        "sandesh-imap",
        "sandesh-pop3",
        "sandesh-proto",
    ] {
        assert!(
            !closure.contains(protocol),
            "sandesh-api must not depend on {protocol}"
        );
    }
}

#[test]
fn the_api_does_not_duplicate_credential_verification() {
    // The API authenticates (ADR-075), and §59 forbids it from reaching
    // `sandesh-proto`, where Argon2id verification lives. The resolution is a
    // port (`sandesh_core::CredentialVerifier`) implemented by the composition
    // root — *not* a second password-checking implementation inside the API,
    // which is how one of the two ends up without the timing-safety property
    // the other has (ADR-032).
    let graph = Graph::load();
    let direct = graph.direct_runtime_deps("sandesh-api");

    for banned in ["argon2", "subtle", "rustls", "rcgen", "rustls-pemfile"] {
        assert!(
            !direct.contains(banned),
            "sandesh-api must not depend on {banned}: credential verification \
             belongs behind `CredentialVerifier`, and TLS configuration belongs \
             to the composition root. `tokio-rustls` is permitted — it only \
             *receives* an acceptor, the same arrangement sandesh-smtp has."
        );
    }

    // `base64` is permitted and deliberately so: it decodes the `Basic` header,
    // which is a transport encoding, not cryptography. The verification it
    // feeds still goes through the port.
    assert!(
        direct.contains("tokio-rustls"),
        "sandesh-api serves HTTPS and should receive an acceptor through tokio-rustls"
    );
}

#[test]
fn smtp_depends_on_no_http_or_websocket() {
    // The other half of §59, checked from the SMTP side: mail delivery must not
    // acquire a dependency on the API being present.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-smtp");

    for banned in ["axum", "axum-core", "hyper", "tower-http", "tungstenite"] {
        assert!(
            !closure.contains(banned),
            "sandesh-smtp depends on {banned}: SMTP must work with HTTP disabled"
        );
    }
    assert!(!closure.contains("sandesh-api"));
}

#[test]
fn storage_depends_on_no_web_layer() {
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-storage");

    for banned in ["axum", "axum-core", "hyper", "tower-http", "tungstenite"] {
        assert!(
            !closure.contains(banned),
            "sandesh-storage depends on {banned}: storage is beneath the web layer"
        );
    }
    assert!(!closure.contains("sandesh-api"));
}

#[test]
fn the_app_is_the_only_composition_root() {
    // ARCHITECTURE §3.1: `sandesh-app` is the one crate permitted to know about
    // all the others. The test is symmetric — the app sees everything, and
    // nothing sees the app.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-app");

    for expected in [
        "sandesh-core",
        "sandesh-storage",
        "sandesh-events",
        "sandesh-config",
        "sandesh-api",
        "sandesh-smtp",
        "sandesh-imap",
        "sandesh-pop3",
    ] {
        assert!(
            closure.contains(expected),
            "the composition root must wire {expected}"
        );
    }

    // Enumerated from the graph rather than from a hand-written list. The list
    // form passed silently when `sandesh-desktop` was added — a new crate could
    // depend on the composition root and no test would notice, which is the
    // opposite of what this test exists to do.
    //
    // ADR-092: `sandesh-desktop` is the **one** permitted exception. A desktop
    // shell must embed the whole application by definition, and asserting it is
    // the only exception keeps the rule sharp for every adapter crate.
    const MAY_DEPEND_ON_THE_COMPOSITION_ROOT: &[&str] = &["sandesh-desktop"];

    for crate_name in graph.sandesh_packages() {
        if crate_name == "sandesh-app"
            || MAY_DEPEND_ON_THE_COMPOSITION_ROOT.contains(&crate_name.as_str())
        {
            continue;
        }
        assert!(
            !graph.runtime_closure(&crate_name).contains("sandesh-app"),
            "{crate_name} depends on sandesh-app: only the composition root may \
             know about the whole application"
        );
    }

    // The exception must stay an exception: if a listed crate stops depending
    // on the app, the entry is stale and the rule has quietly widened.
    for permitted in MAY_DEPEND_ON_THE_COMPOSITION_ROOT {
        assert!(
            graph.runtime_closure(permitted).contains("sandesh-app"),
            "{permitted} is listed as permitted to depend on sandesh-app but does not: \
             remove the exception rather than leaving the rule wider than it needs to be"
        );
    }
}

#[test]
fn only_the_desktop_shell_embeds_the_whole_application() {
    // The symmetric half of ADR-092, stated as its own test so the reason is
    // visible: the desktop shell is a *consumer* of the composition root, not a
    // second one. It must not reach past `sandesh-app` into the crates the app
    // wires — a shell that constructs its own storage or its own listeners is
    // forked wiring, which is exactly what Phase 9's "no forked code" criterion
    // forbids.
    let graph = Graph::load();

    for wired_by_the_app in [
        "sandesh-storage",
        "sandesh-api",
        "sandesh-smtp",
        "sandesh-imap",
        "sandesh-pop3",
        "sandesh-events",
        "sandesh-webhook",
    ] {
        assert!(
            !graph
                .direct_runtime_deps("sandesh-desktop")
                .contains(wired_by_the_app),
            "sandesh-desktop depends directly on {wired_by_the_app}: the shell must reach \
             the engine through sandesh-app, never assemble its own"
        );
    }
}

#[test]
fn the_api_uses_one_http_and_one_websocket_stack() {
    // DEPENDENCIES §: axum plus its own `ws` support, deliberately not a second
    // framework alongside it. Two stacks would mean two middleware chains and
    // two places to get security limits wrong.
    let graph = Graph::load();
    let closure = graph.runtime_closure("sandesh-api");

    assert!(closure.contains("axum"), "the chosen framework is axum");
    for competitor in ["actix-web", "warp", "rocket", "tide", "poem", "salvo"] {
        assert!(
            !closure.contains(competitor),
            "a second HTTP framework ({competitor}) has been introduced"
        );
    }
}

#[test]
fn the_check_can_actually_detect_a_violation() {
    // A test that always passes is worse than no test. This proves the graph
    // walk sees real dependencies: sandesh-storage genuinely depends on sqlx and
    // tokio, so the same check applied there must fail.
    let graph = Graph::load();
    let storage = graph.runtime_closure("sandesh-storage");

    assert!(
        storage.contains("sqlx"),
        "the metadata walk is not seeing real dependencies; \
         sandesh-storage must show sqlx in its closure"
    );
    assert!(
        storage.contains("tokio"),
        "the metadata walk is not seeing transitive dependencies; \
         tokio reaches sandesh-storage through sqlx"
    );

    // And those same crates must be absent from the core's closure.
    let core = graph.runtime_closure("sandesh-core");
    assert!(!core.contains("sqlx"));
    assert!(!core.contains("tokio"));

    // The adapter crates are present in the graph, so the rules above are
    // actually being evaluated rather than passing over absent packages.
    for crate_name in [
        "sandesh-smtp",
        "sandesh-imap",
        "sandesh-pop3",
        "sandesh-proto",
        "sandesh-config",
    ] {
        assert!(
            graph.by_name.contains_key(crate_name),
            "{crate_name} is not in the workspace; its architecture rules would \
              pass vacuously"
        );
    }
}

#[cfg(test)]
mod id_parsing {
    use super::package_name;

    #[test]
    fn handles_both_package_id_shapes() {
        assert_eq!(
            package_name(
                "registry+https://github.com/rust-lang/crates.io-index#async-trait@0.1.92"
            ),
            "async-trait"
        );
        assert_eq!(
            package_name("path+file:///home/user/Sandesh/crates/sandesh-core#0.1.0"),
            "sandesh-core"
        );
        // A path package whose directory name differs from the crate name still
        // renders the `#name@version` form, which the first branch handles.
        assert_eq!(
            package_name("path+file:///home/user/proj#renamed@0.1.0"),
            "renamed"
        );
    }
}
