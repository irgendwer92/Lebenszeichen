/// Commands of the app. Listing them turns each into a permission
/// (`allow-…`), so that a window may only call what its capability in
/// `capabilities/` grants: the main window everything, a foreground window
/// only what concerns its own request.
const COMMANDS: &[&str] = &[
    "platform",
    "setup_state",
    "init_device",
    "init_with_invitation_only",
    "init_from_setup_code",
    "setup_code",
    "code_kind",
    "devices",
    "device_rename",
    "device_remove",
    "device_join",
    "device_add",
    "device_confirm",
    "device_cancel",
    "overview",
    "sync",
    "invite",
    "accept",
    "confirm",
    "reject",
    "request",
    "respond",
    "remove",
    "set_rules",
    "set_report_setting",
    "clear_state",
    "report_token",
    "enable_report_token",
    "renew_report_token",
    "disable_report_token",
    "texts",
    "set_language",
    "autostart",
    "set_autostart",
    "alert_text",
    "alert_answer",
];

fn main() {
    // tauri-build does not watch the platform-specific configuration, so a
    // change to it alone would not rebuild the app with the new settings.
    println!("cargo:rerun-if-changed=tauri.linux.conf.json");
    println!("cargo:rerun-if-changed=tauri.windows.conf.json");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
