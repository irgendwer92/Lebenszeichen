//! Client layer shared by the command-line client `lz` and the desktop app:
//! relay access, the commands, and fetching messages.
//!
//! The protocol itself lives in `lz-core`; this crate adds the network, the
//! state file and the texts for people, in English and German. Commands are functions on
//! [`ctx::Ctx`], so tests can call them with a fake relay and a manual clock.

#[macro_use]
pub mod i18n;
#[macro_use]
pub mod ctx;
pub mod devices;
pub mod push;
pub mod qr;
pub mod relay;
pub mod show;
pub mod sync;

#[cfg(test)]
mod tests;

/// Installs `ring` as the TLS provider; see Cargo.toml. Call once at
/// program start, before the first connection.
pub fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
