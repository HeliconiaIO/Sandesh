//! Tauri's build step: generates the context `tauri::generate_context!` reads.
use std::path::Path;

fn main() {
    // `generate_context!` fails the build when `frontendDist` does not exist,
    // and unlike `rust-embed` in `sandesh-api` it has no `allow_missing`. That
    // would make `npm run build` a prerequisite for `cargo check --workspace`
    // in a clean checkout, which `sandesh-api/src/assets.rs` deliberately
    // avoids. An empty directory satisfies the check; a desktop build meant to
    // ship still builds the UI first, as the release scripts do.
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if !dist.exists() {
        let _ = std::fs::create_dir_all(&dist);
    }
    tauri_build::build();
}
