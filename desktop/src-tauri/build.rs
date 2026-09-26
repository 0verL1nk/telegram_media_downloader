fn main() {
    // These commands are granted to the remote Telegram WebView capability: it may start a
    // page-fetch download, plan/push/finish/fail it, and query task state by file name.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "start_webview_download",
            "plan_chunks",
            "push_chunk",
            "finish_download",
            "fail_download",
            "webview_query_task_state",
        ]),
    ))
    .expect("failed to generate Tauri permissions");

    build_webview_inject();
}

/// Regenerate `webview-inject/dist/inject.js` when sources are newer (or dist is missing).
fn build_webview_inject() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root = std::path::Path::new(&manifest_dir);
    let build_mjs = root.join("webview-inject/build.mjs");
    if !build_mjs.exists() {
        return;
    }
    let src_dir = root.join("webview-inject/src");
    let dist = root.join("webview-inject/dist/inject.js");

    let stale = !dist.exists()
        || std::fs::read_dir(&src_dir)
            .ok()
            .map(|entries| {
                entries.flatten().any(|e| {
                    e.metadata().and_then(|m| m.modified()).ok()
                        > dist.metadata().and_then(|m| m.modified()).ok()
                })
            })
            .unwrap_or(false);

    if stale {
        match std::process::Command::new("node").arg(&build_mjs).status() {
            Ok(status) if status.success() => {}
            _ if dist.exists() => {
                println!(
                    "cargo:warning=webview-inject rebuild failed; using existing dist/inject.js"
                );
            }
            _ => {
                panic!(
                    "webview-inject: node build failed and dist/inject.js is missing; \
                     install Node.js and rebuild"
                );
            }
        }
    }

    // Also watch the generated bundle: it is git-ignored, so deleting it (a
    // fresh checkout, `git clean -xdf`, or a manual `rm`) must re-trigger this
    // build script, otherwise `include_str!` fails with a missing-file error.
    println!("cargo:rerun-if-changed=webview-inject/src");
    println!("cargo:rerun-if-changed=webview-inject/config.json");
    println!("cargo:rerun-if-changed=webview-inject/build.mjs");
    println!("cargo:rerun-if-changed=webview-inject/dist/inject.js");
}
