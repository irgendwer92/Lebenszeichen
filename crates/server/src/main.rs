//! `lz-server`: Lebenszeichen as a service for a container or a Home
//! Assistant app (`docs/m8b-container.md`).
//!
//! One process listens like `lz listen` and sends due reported answers,
//! serves the local interface on a socket and optionally on a TCP port,
//! both with the token, and optionally serves the setup page with its JSON
//! API to a fixed list of peers.

use std::process::ExitCode;

#[cfg(unix)]
mod cli;
#[cfg(unix)]
mod join;
#[cfg(unix)]
mod page;
#[cfg(unix)]
mod server;
#[cfg(all(test, unix))]
mod tests;

#[cfg(unix)]
fn main() -> ExitCode {
    cli::main()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("lz-server: only Linux and other Unix systems are supported");
    ExitCode::FAILURE
}
