//! Lebenszeichen client without user interface. Program name: `lz`.
//!
//! Output and help texts are German; the commands live in [`app`] and
//! [`sync`], callable from tests with a fake relay.

use std::process::ExitCode;

mod app;
mod cli;
mod relay;
mod show;
mod sync;

#[cfg(test)]
mod tests;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // TLS with ring only; see Cargo.toml.
    let _ = rustls::crypto::ring::default_provider().install_default();
    match cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lz: {error}");
            ExitCode::FAILURE
        }
    }
}
