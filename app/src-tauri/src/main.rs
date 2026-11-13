// Desktop entry point. Unused in M1, the prototype only targets Android.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lebenszeichen_lib::run();
}
