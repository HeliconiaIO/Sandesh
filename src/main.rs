//! The `sandesh` binary at the workspace root, so that bare `cargo run` works
//! from the repository root — the command the README promises a developer.
//!
//! The implementation is `sandesh_app::run_cli`; this is only the entry point.

fn main() -> std::process::ExitCode {
    sandesh_app::run_cli()
}
