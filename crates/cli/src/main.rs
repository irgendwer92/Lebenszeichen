//! Lebenszeichen client without user interface. Program name: `lz`.
//!
//! Output and help texts are German. The commands live in `lz-client`,
//! which the desktop app shares; this crate only parses arguments.

use std::process::ExitCode;

mod cli;
mod hints;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    lz_client::install_tls_provider();
    match cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lz: {error}");
            ExitCode::FAILURE
        }
    }
}
