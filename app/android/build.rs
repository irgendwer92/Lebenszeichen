/// Commands implemented by the Kotlin class `LebenszeichenPlugin`.
///
/// The interface calls them as `plugin:lebenszeichen|<name>`; Tauri turns the
/// names into camel case before it looks up the Kotlin method.
const COMMANDS: &[&str] = &[
    "status",
    "register",
    "unregister",
    "set_answer_url",
    "trigger_local",
    "request_notification_permission",
    "open_settings",
    "copy_text",
    "read_log",
    "clear_log",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("kotlin")
        .build();
}
