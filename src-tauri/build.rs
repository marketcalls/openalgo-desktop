fn main() {
    // App commands get explicit permissions (`allow-<command>`), so each one
    // is reachable only from a capability that names it: the bundled
    // start-up page (`default`) or the app's own server page (`remote-app`).
    let app = tauri_build::AppManifest::new().commands(&[
        "startup_status",
        "retry_server",
        "restart_server",
        "open_external",
        "reset_account",
    ]);
    if let Err(e) = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(app)) {
        panic!("tauri build failed: {e:#}");
    }
}
