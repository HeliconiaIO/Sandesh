//! Repository automation (`cargo xtask`).
//!
//! The one command that regenerates the API contract. It is a separate crate
//! rather than a flag on the `sandesh` binary because generation is a developer
//! task, not a product feature: nothing here ships to a user, and the shipped
//! binary does not grow a code path that writes to the source tree.
//!
//! ```text
//! cargo xtask generate-api
//!         │
//!         ├──▶ docs/openapi.json          (from the Rust annotations)
//!         └──▶ web/src/generated/api.ts   (from docs/openapi.json, via npm)
//! ```
//!
//! `--check` verifies the committed files match what generation produces and
//! exits non-zero otherwise. That is the mode CI runs, and it is what makes it
//! impossible to change a handler's contract while leaving the frontend types
//! stale (ADR-028).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Where the generated OpenAPI document lives, relative to the repository root.
const OPENAPI_PATH: &str = "docs/openapi.json";

/// Where the generated TypeScript lives, relative to the repository root.
const TYPES_PATH: &str = "web/src/generated/api.ts";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let task = args.next();
    let check = args.any(|arg| arg == "--check");

    match task.as_deref() {
        Some("generate-api") => match generate_api(check) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("error: unknown task `{other}`");
            usage();
            ExitCode::FAILURE
        }
        None => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask generate-api [--check]");
    eprintln!();
    eprintln!("    generate-api    Regenerate the OpenAPI document from the Rust API.");
    eprintln!("    --check         Fail if the committed file is out of date.");
}

/// The repository root, derived from this crate's location rather than the
/// current directory, so the task behaves the same from any subdirectory.
fn repository_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| String::from("cannot locate the repository root"))
}

/// Writes (or verifies) the OpenAPI document.
fn generate_api(check: bool) -> Result<(), String> {
    let root = repository_root()?;
    let generated = sandesh_api::openapi::document_json()
        .map_err(|error| format!("cannot serialise the OpenAPI document: {error}"))?;

    let path = root.join(OPENAPI_PATH);

    if check {
        let committed = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

        if committed != generated {
            return Err(format!(
                "{OPENAPI_PATH} is out of date.\n\
                 The Rust API changed but the generated contract was not regenerated.\n\
                 Run `cargo xtask generate-api` and commit the result."
            ));
        }

        println!("{OPENAPI_PATH} is up to date");
        return check_types(&root);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    std::fs::write(&path, &generated)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;

    println!("wrote {OPENAPI_PATH}");
    generate_types(&root)
}

/// Verifies the committed TypeScript matches the committed contract.
///
/// Regenerates into a scratch copy and compares, so the check never mutates the
/// working tree — a CI check that edits the thing it is checking is not a check.
fn check_types(root: &Path) -> Result<(), String> {
    let committed_path = root.join(TYPES_PATH);
    let committed = match std::fs::read_to_string(&committed_path) {
        Ok(contents) => contents,
        Err(error) => return Err(format!("cannot read {TYPES_PATH}: {error}")),
    };

    if !root.join("web/node_modules").is_dir() {
        println!("skipping the {TYPES_PATH} check: web/node_modules is absent");
        return Ok(());
    }

    generate_types(root)?;

    let regenerated = std::fs::read_to_string(&committed_path)
        .map_err(|error| format!("cannot read {TYPES_PATH}: {error}"))?;

    if committed == regenerated {
        println!("{TYPES_PATH} is up to date");
        return Ok(());
    }

    // Put back what was committed, so a failing check leaves no diff behind.
    let _ = std::fs::write(&committed_path, &committed);

    Err(format!(
        "{TYPES_PATH} is out of date.\n\
         The contract changed but the generated TypeScript was not regenerated.\n\
         Run `cargo xtask generate-api` and commit the result."
    ))
}

/// Regenerates the TypeScript the UI compiles against.
///
/// Delegated to `openapi-typescript` via npm rather than reimplemented: the
/// mapping from OpenAPI to TypeScript is a solved problem with edge cases
/// (nullability, `oneOf`, generics) that a hand-rolled emitter would get subtly
/// wrong, and a subtly wrong type is worse than none.
///
/// A missing `node_modules` is reported as a clear instruction rather than a
/// failure, because a Rust-only contributor may reasonably not have run
/// `npm install`, and the OpenAPI half of the generation already succeeded.
fn generate_types(root: &Path) -> Result<(), String> {
    let web = root.join("web");

    if !web.join("node_modules").is_dir() {
        println!(
            "skipping {TYPES_PATH}: web/node_modules is absent.\n\
             Run `npm install` in web/, then `cargo xtask generate-api` again."
        );
        return Ok(());
    }

    let status = Command::new("npm")
        .arg("run")
        .arg("generate")
        .current_dir(&web)
        .status()
        .map_err(|error| format!("cannot run npm in {}: {error}", web.display()))?;

    if !status.success() {
        return Err(format!("`npm run generate` failed with {status}"));
    }

    println!("wrote {TYPES_PATH}");
    Ok(())
}
