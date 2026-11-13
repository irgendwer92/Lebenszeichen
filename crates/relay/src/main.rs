//! Relay server: accepts ciphertext, hands it on and deletes it.
//!
//! The relay knows neither names nor keys nor message types.

fn main() {
    println!(
        "lz-relay {} (Protokollversion {})",
        env!("CARGO_PKG_VERSION"),
        lz_wire::PROTOCOL_VERSION
    );
    println!("Noch ohne Funktion. Das Relay folgt in Meilenstein M2.");
}
