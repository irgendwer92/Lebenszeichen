fn main() {
    // tauri-build does not watch the platform-specific configuration, so a
    // change to it alone would not rebuild the app with the new settings.
    println!("cargo:rerun-if-changed=tauri.linux.conf.json");
    tauri_build::build();
}
