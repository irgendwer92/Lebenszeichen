// Desktop entry point (M3). On Android, Tauri starts the library instead.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lebenszeichen_lib::run();
}
