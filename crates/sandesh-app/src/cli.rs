//! The command-line entry point.
//!
//! Deliberately thin: parse a little configuration, start the application, wait
//! for a signal, stop. Everything of substance is in the library, which is what
//! lets the integration tests start a real Sandesh in-process on ephemeral
//! ports rather than shelling out to a binary and scraping its output.
//!
//! There is no `unwrap`, `expect` or `panic` on this path. A startup failure
//! prints one clear line and exits non-zero.

use std::process::ExitCode;

use crate::{Application, logging};
use sandesh_config::AppConfig;

/// Runs Sandesh from the command line.
///
/// Lives in the library so both the `sandesh-app` binary and the workspace-root
/// binary that makes bare `cargo run` work share one implementation.
#[must_use]
pub fn run_cli() -> ExitCode {
    // Argument handling before anything is opened. Sandesh is configured by
    // environment variable (OD-24) and has no flags to parse, but a *downloaded*
    // binary must still be able to say what it is: the first thing anyone does
    // with an unknown executable is ask it, and starting a mail server instead —
    // which is what this did until Phase 10 — is a poor answer. Hand-matched
    // rather than pulling in an argument parser for two strings.
    //
    // Every argument is validated before any is acted on, so that
    // `sandesh --version --nonsense` reports the bad argument rather than
    // exiting 0 on the good one and never looking at the rest. Clippy caught
    // that: the first version of this returned from inside the loop.
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let is_workspace_cmd = arguments.first().is_some_and(|a| a == "workspace");
    let is_purge_cmd = arguments.first().is_some_and(|a| a == "purge");

    if !is_workspace_cmd && !is_purge_cmd {
        if let Some(unrecognised) = arguments
            .iter()
            .find(|argument| !matches!(argument.as_str(), "--version" | "-V" | "--help" | "-h"))
        {
            eprintln!("sandesh: unrecognised argument: {unrecognised}");
            eprintln!("sandesh: try --help; Sandesh is configured by environment variable");
            return ExitCode::FAILURE;
        }

        // `--help` wins over `--version` when both are given: it is the more
        // informative answer, and it contains the version anyway.
        if arguments.iter().any(|a| a == "--help" || a == "-h") {
            print!("{}", help_text());
            return ExitCode::SUCCESS;
        }

        if arguments.iter().any(|a| a == "--version" || a == "-V") {
            println!("sandesh {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
    }

    // The runtime is built here rather than with `#[tokio::main]` so that a
    // failure to build it is reported like any other startup failure instead of
    // panicking out of a generated main.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("sandesh: cannot start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };

    if is_workspace_cmd {
        return runtime.block_on(run_workspace_cli(&arguments[1..]));
    }

    if is_purge_cmd {
        return runtime.block_on(run_purge_cli(&arguments[1..]));
    }

    runtime.block_on(run())
}

/// The `--help` output.
fn help_text() -> String {
    format!(
        "\
sandesh {version} — a local mail server for developers.

Usage:
  sandesh                          Start SMTP, IMAP, POP3 and the HTTP API with the UI.
  sandesh workspace list           List all workspaces.
  sandesh workspace use <id>       Switch active workspace.
  sandesh workspace create <id> [name] [--clone-from <id>]  Create a new isolated workspace.
  sandesh workspace delete <id>    Delete a workspace and its database.
  sandesh purge [--factory-reset]  Purge messages and traces from active workspace.
  sandesh --version                Print the version and exit.
  sandesh --help                   Print this message and exit.

Sandesh is configured by environment variables:
  SANDESH_DATA_DIR        Where mail is kept (default: current working directory).
  SANDESH_SMTP_ADDR       SMTP listener        (default: 127.0.0.1:1025).
  SANDESH_IMAP_ADDR       IMAP listener        (default: 127.0.0.1:1143).
  SANDESH_HTTP_ADDR       HTTP API and web UI  (default: 127.0.0.1:8080).
  SANDESH_LOG             Log filter           (default: info).

Everything binds to loopback and authentication is off by default; see README.md
for the full set, including TLS and API authentication.
",
        version = env!("CARGO_PKG_VERSION")
    )
}

async fn init_workspace_manager() -> Result<sandesh_storage::WorkspaceManager, ExitCode> {
    let config = match crate::env::apply(AppConfig::local()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sandesh: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let data_dir = config.database_path.parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );
    let events = sandesh_storage::EventRecorder::new(std::sync::Arc::new(
        sandesh_events::EventBus::with_capacity(16),
    ));
    sandesh_storage::WorkspaceManager::load_or_init(
        data_dir,
        config.database_path,
        config.blob_path,
        events,
    )
    .await
    .map_err(|e| {
        eprintln!("sandesh: cannot load workspaces: {e}");
        ExitCode::FAILURE
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "a flat subcommand dispatch reads best in one place"
)]
async fn run_workspace_cli(args: &[String]) -> ExitCode {
    let sub = args.first().map_or("list", String::as_str);
    match sub {
        "list" => {
            let Ok(mgr) = init_workspace_manager().await else {
                return ExitCode::FAILURE;
            };
            let list = match mgr.list().await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("sandesh: failed to list workspaces: {e}");
                    return ExitCode::FAILURE;
                }
            };
            println!(
                "{:<2} {:<16} {:<24} {:<10} {:<10}",
                "  ", "ID", "NAME", "MESSAGES", "SIZE"
            );
            for ws in list {
                let marker = if ws.is_active { "*" } else { " " };
                #[allow(clippy::cast_precision_loss, reason = "display only")]
                let size_kb = (ws.size_bytes as f64) / 1024.0;
                let size_str = if size_kb > 1024.0 {
                    format!("{:.1} MB", size_kb / 1024.0)
                } else {
                    format!("{size_kb:.1} KB")
                };
                println!(
                    "{marker:<2} {:<16} {:<24} {:<10} {size_str:<10}",
                    ws.id, ws.name, ws.message_count
                );
            }
            ExitCode::SUCCESS
        }
        "use" => {
            let Some(id) = args.get(1) else {
                eprintln!(
                    "sandesh: workspace use requires a workspace ID: sandesh workspace use <id>"
                );
                return ExitCode::FAILURE;
            };
            let Ok(mgr) = init_workspace_manager().await else {
                return ExitCode::FAILURE;
            };
            if let Err(e) = mgr.set_active(id).await {
                eprintln!("sandesh: failed to set active workspace: {e}");
                return ExitCode::FAILURE;
            }
            println!("Active workspace set to '{id}'.");
            ExitCode::SUCCESS
        }
        "create" => {
            let mut id = None;
            let mut name = None;
            let mut clone_from = None;
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--clone-from"
                    && let Some(src) = args.get(i + 1)
                {
                    clone_from = Some(src.as_str());
                    i += 2;
                    continue;
                }
                if id.is_none() {
                    id = Some(args[i].as_str());
                } else if name.is_none() {
                    name = Some(args[i].as_str());
                }
                i += 1;
            }

            let Some(id) = id else {
                eprintln!(
                    "sandesh: workspace create requires an ID: sandesh workspace create <id> [name] [--clone-from <id>]"
                );
                return ExitCode::FAILURE;
            };
            let name = name.unwrap_or(id);
            let mgr = match init_workspace_manager().await {
                Ok(m) => m,
                Err(c) => return c,
            };
            match mgr.create(id, name, None, clone_from, None, false).await {
                Ok(ws) => {
                    if let Some(src) = clone_from {
                        println!(
                            "Created workspace '{}' (id: {}) cloned from '{src}'.",
                            ws.name, ws.id
                        );
                    } else {
                        println!("Created workspace '{}' (id: {}).", ws.name, ws.id);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("sandesh: failed to create workspace: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "delete" => {
            let Some(id) = args.get(1) else {
                eprintln!(
                    "sandesh: workspace delete requires a workspace ID: sandesh workspace delete <id>"
                );
                return ExitCode::FAILURE;
            };
            if id == "default" {
                eprintln!("sandesh: cannot delete the default workspace");
                return ExitCode::FAILURE;
            }
            let Ok(mgr) = init_workspace_manager().await else {
                return ExitCode::FAILURE;
            };
            match mgr.delete(id).await {
                Ok(true) => {
                    println!("Deleted workspace '{id}'.");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!("sandesh: workspace '{id}' not found");
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("sandesh: failed to delete workspace: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "purge" => {
            let Some(id) = args.get(1) else {
                eprintln!(
                    "sandesh: workspace purge requires a workspace ID: sandesh workspace purge <id>"
                );
                return ExitCode::FAILURE;
            };
            let factory_reset = args.iter().any(|a| a == "--factory-reset");
            let Ok(mgr) = init_workspace_manager().await else {
                return ExitCode::FAILURE;
            };
            let options = sandesh_core::PurgeOptions {
                mode: if factory_reset {
                    sandesh_core::PurgeMode::FactoryReset
                } else {
                    sandesh_core::PurgeMode::MailAndTraces
                },
                clear_traces: true,
                clear_webhooks: true,
            };
            match mgr.purge(id, options).await {
                Ok(res) => {
                    println!(
                        "Purged workspace '{}': {} messages, {} traces deleted (reclaimed {} bytes).",
                        res.workspace_id,
                        res.deleted_messages,
                        res.deleted_traces,
                        res.reclaimed_bytes
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("sandesh: failed to purge workspace: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("sandesh: unrecognised workspace command: {other}");
            eprintln!("sandesh: available commands: list, use, create, delete, purge");
            ExitCode::FAILURE
        }
    }
}

async fn run_purge_cli(args: &[String]) -> ExitCode {
    let factory_reset = args.iter().any(|a| a == "--factory-reset");
    let Ok(mgr) = init_workspace_manager().await else {
        return ExitCode::FAILURE;
    };
    let active_id = mgr.active_id().await;
    let options = sandesh_core::PurgeOptions {
        mode: if factory_reset {
            sandesh_core::PurgeMode::FactoryReset
        } else {
            sandesh_core::PurgeMode::MailAndTraces
        },
        clear_traces: true,
        clear_webhooks: true,
    };
    match mgr.purge(&active_id, options).await {
        Ok(res) => {
            println!(
                "Purged active workspace '{}': {} messages, {} traces deleted (reclaimed {} bytes).",
                res.workspace_id, res.deleted_messages, res.deleted_traces, res.reclaimed_bytes
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("sandesh: failed to purge workspace: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Starts, serves, and stops.
async fn run() -> ExitCode {
    // Defaults first, then the environment on top (OD-24). A bad variable is
    // reported before anything is opened, so a typo fails immediately with the
    // variable's name rather than silently taking no effect.
    let config = match crate::env::apply(AppConfig::local()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("sandesh: {error}");
            return ExitCode::FAILURE;
        }
    };

    logging::init(&config.log_level);

    let application = match Application::start(config).await {
        Ok(application) => application,
        Err(error) => {
            // Startup failures go to stderr as well as the log: the log level
            // may be filtering the tracing output, and a developer running
            // `cargo run` must always be told why nothing started.
            eprintln!("sandesh: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("{}", application.banner());

    let shutdown = application.shutdown_token();
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received");
        shutdown.cancel();
    });

    match application.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sandesh: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Waits for the platform's termination signals.
///
/// Ctrl-C everywhere, plus `SIGTERM` on Unix, which is what a container runtime
/// or `systemctl stop` sends — ignoring it would mean being killed rather than
/// shutting down cleanly.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                // Without SIGTERM handling Ctrl-C still works; say so and
                // continue rather than refusing to run.
                tracing::warn!(%error, "cannot listen for SIGTERM; Ctrl-C still works");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
