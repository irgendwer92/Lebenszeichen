//! Command-line client without a user interface.
//!
//! Used to test the protocol before any app exists, and later as the client
//! for devices without a screen.

fn main() {
    println!(
        "lz {} (Protokollversion {})",
        env!("CARGO_PKG_VERSION"),
        lz_core::wire::PROTOCOL_VERSION
    );
    println!("Noch ohne Funktion. Das Protokoll folgt in Meilenstein M2.");
}
