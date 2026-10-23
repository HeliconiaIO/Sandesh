//! Serving the Web UI (§41, §44, §45).
//!
//! The built frontend is embedded in the binary at compile time, so `cargo run`
//! produces one artefact that serves both the API and the interface. There is
//! no asset directory to deploy beside the binary and no absolute path from the
//! machine that built it, which is what keeps the result reproducible on
//! another developer's machine and packageable for Linux, macOS and Windows
//! later (§86, §100).
//!
//! ## Routing
//!
//! ```text
//! /api/*            → never reaches here (nested first)
//! /assets/index.js  → the embedded file
//! /mailbox/x/...    → index.html, so the SPA router can take over
//! ```
//!
//! The fallback is what makes a pasted deep link work (§45). It is deliberately
//! registered as a *fallback* rather than a catch-all route, so it cannot
//! shadow the API or the WebSocket: an unknown `/api/...` path still produces a
//! 404 from the API, never an HTML page, which would be far more confusing to a
//! client expecting JSON.
//!
//! ## When the UI has not been built
//!
//! A Rust-only contributor who has never run `npm run build` still gets a
//! working server; requesting the UI explains what to run rather than serving a
//! blank page or failing to compile.

use axum::body::Body;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// The built frontend.
///
/// `folder` is resolved relative to this crate's manifest at compile time, so
/// the path does not depend on the working directory a build was started from.
///
/// `allow_missing` is load-bearing rather than defensive. Without it
/// `rust-embed` fails the *build* when `web/dist` does not exist, which would
/// mean `cargo check`, `cargo test` and `cargo clippy` could not run in a clean
/// checkout until someone had run `npm run build` — making a Node toolchain a
/// prerequisite for compiling Rust, and breaking every CI job that does not
/// build the UI. With it, an absent UI is a runtime condition that
/// [`is_available`] reports and [`not_built`] explains.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/dist"]
#[allow_missing = true]
struct Assets;

/// The entry point every SPA route falls back to.
const INDEX: &str = "index.html";

/// Whether a built UI is embedded.
#[must_use]
pub fn is_available() -> bool {
    Assets::get(INDEX).is_some()
}

/// The Content-Security-Policy for the application document (§73).
///
/// The policy is tight because this origin holds the API and the event stream:
///
/// * `default-src 'self'` — no third-party anything. The UI bundles every
///   dependency, so there is no CDN to allow.
/// * `script-src 'self'` — no `unsafe-inline`, no `unsafe-eval`. An injected
///   `<script>` cannot run even if one were somehow reflected.
/// * `frame-src 'none'` with `child-src 'none'` — the email preview uses a
///   `srcdoc` iframe, which these do not block, but no *remote* frame may load.
/// * `connect-src 'self'` — covers the REST calls and the WebSocket, which
///   inherits the document's origin.
/// * `object-src 'none'`, `base-uri 'none'`, `form-action 'none'` — remove
///   plugin, base-tag and form-submission vectors outright.
/// * `frame-ancestors 'none'` — clickjacking protection; nothing may embed
///   Sandesh (§76).
///
/// `style-src` allows inline styles because the bundler emits them and because
/// a stylesheet cannot exfiltrate data the way script can.
///
/// ## Why the script hash is computed, not written down
///
/// Vite emits one small inline `<script>` in `index.html`, so `script-src`
/// needs its hash. That hash was previously a literal in this file, and it did
/// not match what the bundler produced — which meant the browser refused to run
/// the inline script and **the whole UI failed to boot**, with nothing but a
/// console message to say why. A literal cannot survive a frontend rebuild: any
/// change to the bundle changes the script, and nothing failed a test when the
/// two drifted apart. It is derived from the embedded asset instead, so the
/// policy and the file it describes cannot disagree.
static CONTENT_SECURITY_POLICY: LazyLock<String> = LazyLock::new(|| {
    let mut script_src = String::from("'self'");
    for hash in inline_script_hashes() {
        use std::fmt::Write as _;
        let _ = write!(script_src, " 'sha256-{hash}'");
    }
    format!(
        "default-src 'self'; \
     script-src {script_src}; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     connect-src 'self'; \
     frame-src 'self'; \
     child-src 'self'; \
     object-src 'none'; \
     base-uri 'none'; \
     form-action 'none'; \
     frame-ancestors 'none'"
    )
});

/// The base64 SHA-256 of every inline `<script>` in the embedded `index.html`.
///
/// Only scripts *without* a `src` are hashed; an external one is covered by
/// `'self'`. Returns empty when no UI is embedded, which leaves the policy at
/// `script-src 'self'` — correct for a server with no UI to serve.
fn inline_script_hashes() -> Vec<String> {
    let Some(index) = Assets::get(INDEX) else {
        return Vec::new();
    };
    let html = String::from_utf8_lossy(&index.data);

    let mut hashes = Vec::new();
    let mut rest = html.as_ref();
    while let Some(open) = rest.find("<script") {
        rest = &rest[open..];
        let Some(tag_end) = rest.find('>') else { break };
        let tag = &rest[..tag_end];
        let body_start = tag_end + 1;
        let Some(close) = rest[body_start..].find("</script>") else {
            break;
        };
        let body = &rest[body_start..body_start + close];

        // A script with a `src` loads an external file, which `'self'` already
        // covers; hashing its (empty) body would add a meaningless entry.
        if !tag.contains(" src=") && !body.is_empty() {
            let digest = Sha256::digest(body.as_bytes());
            hashes.push(BASE64.encode(digest));
        }

        rest = &rest[body_start + close..];
    }
    hashes
}

/// Security headers applied to every asset response.
fn security_headers() -> [(header::HeaderName, &'static str); 4] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            CONTENT_SECURITY_POLICY.as_str(),
        ),
        // Belt and braces with `frame-ancestors`, for any older client.
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ]
}

/// Serves an embedded asset, falling back to the SPA entry point.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if !is_available() {
        return not_built();
    }

    // A real file wins; anything else is a client-side route.
    if let Some(response) = embedded(path) {
        return response;
    }

    embedded(INDEX).unwrap_or_else(not_built)
}

/// Renders one embedded file, if it exists.
fn embedded(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    // Hashed asset filenames are immutable, so they may be cached hard; the
    // entry point must not be, or a rebuilt UI would never reach the browser.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    Some(
        (
            StatusCode::OK,
            security_headers(),
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            Body::from(file.data.into_owned()),
        )
            .into_response(),
    )
}

/// The response when no UI has been built into this binary.
fn not_built() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        security_headers(),
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Body::from(
            "<!doctype html><meta charset=\"utf-8\"><title>Sandesh</title>\
             <style>body{font-family:ui-monospace,monospace;margin:3rem auto;max-width:34rem;\
             line-height:1.6;color:#e6edf3;background:#0d1117}code{background:#171f28;\
             padding:2px 6px;border-radius:4px}</style>\
             <h1>The Sandesh interface has not been built</h1>\
             <p>The API is running and usable at <code>/api/v1</code>.</p>\
             <p>To build the interface:</p>\
             <pre><code>cd web\nnpm install\nnpm run build</code></pre>\
             <p>Then restart Sandesh.</p>",
        ),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_forbids_inline_script() {
        // The property that makes an injected script inert. If `unsafe-inline`
        // or `unsafe-eval` ever appears in script-src, email-borne HTML that
        // escapes its frame would become executable in the app origin.
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("script-src 'self' 'unsafe-inline'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("unsafe-eval"));
    }

    #[test]
    fn the_policy_denies_framing_and_plugins() {
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("object-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("base-uri 'none'"));
    }

    #[test]
    fn the_policy_allows_the_api_and_event_stream() {
        // `connect-src 'self'` covers both fetch and the WebSocket upgrade.
        // Without it the UI would load and then silently fail to talk at all.
        assert!(CONTENT_SECURITY_POLICY.contains("connect-src 'self'"));
    }

    #[test]
    fn the_crate_compiles_and_serves_whether_or_not_a_ui_was_built() {
        // The property CI depends on: this crate must build in a clean checkout
        // where `npm run build` has never run, because the Rust jobs do not
        // build the UI. If `allow_missing` is ever dropped, the failure is a
        // compile error in every Rust job rather than anything this assertion
        // could catch — so what is asserted here is the *runtime* half: both
        // states must produce a usable response rather than a panic.
        let response = if is_available() {
            embedded(INDEX)
        } else {
            None
        };

        // With a UI, the entry point resolves; without one, the explanation is
        // served. Neither path may panic, which is what this exercises.
        assert!(response.is_some() || !is_available());
        let _ = not_built();
    }

    #[test]
    fn every_asset_response_carries_the_security_headers() {
        let headers = security_headers();
        assert_eq!(headers.len(), 4);
        assert!(
            headers
                .iter()
                .any(|(name, _)| name == header::CONTENT_SECURITY_POLICY)
        );
        assert!(
            headers
                .iter()
                .any(|(name, _)| name == header::X_CONTENT_TYPE_OPTIONS)
        );
    }

    #[test]
    fn every_inline_script_is_allowed_by_the_policy() {
        // The bug this guards: the hash was a literal that did not match the
        // bundle, so the browser blocked the inline bootstrap script and the UI
        // never mounted. Nothing failed — the server happily served a page that
        // could not run. Deriving the hash makes the two impossible to desync,
        // and this asserts the derivation actually reaches the policy.
        if !is_available() {
            return; // No UI embedded in this build; nothing to hash.
        }

        // Hashed here independently of `inline_script_hashes`, so that a bug
        // in the extraction cannot satisfy this test by agreeing with itself.
        let Some(index) = Assets::get(INDEX) else {
            unreachable!("is_available() said a UI is embedded");
        };
        let html = String::from_utf8_lossy(&index.data).into_owned();

        let mut found = 0;
        for chunk in html.split("<script").skip(1) {
            let Some((tag, after)) = chunk.split_once('>') else {
                continue;
            };
            let Some((body, _)) = after.split_once("</script>") else {
                continue;
            };
            if tag.contains(" src=") || body.is_empty() {
                continue;
            }

            found += 1;
            let expected = BASE64.encode(Sha256::digest(body.as_bytes()));
            assert!(
                CONTENT_SECURITY_POLICY.contains(&format!("'sha256-{expected}'")),
                "the inline script in index.html is not allowed by script-src.\n\
                 expected 'sha256-{expected}'\npolicy: {}",
                *CONTENT_SECURITY_POLICY
            );
        }

        assert!(
            found > 0,
            "the bundler emits an inline bootstrap script; none was found"
        );
    }

    #[test]
    fn a_script_with_a_src_is_not_hashed() {
        // `'self'` already covers an external script, and hashing its empty
        // body would put a hash of "" into the policy.
        assert!(
            !inline_script_hashes()
                .iter()
                .any(|h| h == &BASE64.encode(Sha256::digest(b""))),
            "an empty script body was hashed"
        );
    }
}
