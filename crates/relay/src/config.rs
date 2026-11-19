//! Configuration from environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::ids::{self, Token};
use crate::push::PushPolicy;

/// Settings of the relay, see `docs/wire-format.md`, section "Betrieb".
#[derive(Debug, Clone)]
pub struct Config {
    /// The access token (`LZ_ACCESS_TOKEN`).
    pub access_token: Token,
    /// Address and port to listen on (`LZ_LISTEN`).
    pub listen: SocketAddr,
    /// Path of the SQLite file (`LZ_DB`).
    pub db: PathBuf,
    /// Allowed hosts for wake signals (`LZ_PUSH_ALLOWED_HOSTS`).
    pub push_policy: PushPolicy,
}

impl Config {
    /// Default for `LZ_LISTEN`.
    pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
    /// Default for `LZ_DB`.
    pub const DEFAULT_DB: &str = "/data/relay.sqlite";

    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup`, which returns the value of
    /// a variable or `None` if it is unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let token = lookup("LZ_ACCESS_TOKEN").ok_or("LZ_ACCESS_TOKEN is missing")?;
        let access_token =
            ids::decode(token.trim()).ok_or("LZ_ACCESS_TOKEN must be 32 bytes in b64url")?;
        let listen = lookup("LZ_LISTEN")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| Self::DEFAULT_LISTEN.to_owned());
        let listen = listen
            .trim()
            .parse()
            .map_err(|_| "LZ_LISTEN must be an address and port, such as 0.0.0.0:8080")?;
        let db = lookup("LZ_DB")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| Self::DEFAULT_DB.to_owned())
            .into();
        let push_policy =
            PushPolicy::from_list(&lookup("LZ_PUSH_ALLOWED_HOSTS").unwrap_or_default());
        Ok(Self {
            access_token,
            listen,
            db,
            push_policy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_errors() {
        let token = ids::encode(&[7u8; 32]);
        let config =
            Config::from_lookup(|name| (name == "LZ_ACCESS_TOKEN").then(|| token.clone())).unwrap();
        assert_eq!(config.access_token, [7u8; 32]);
        assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(config.db, PathBuf::from("/data/relay.sqlite"));

        assert!(Config::from_lookup(|_| None).is_err());
        assert!(Config::from_lookup(|_| Some("short".into())).is_err());
        let bad_listen = Config::from_lookup(|name| match name {
            "LZ_ACCESS_TOKEN" => Some(token.clone()),
            "LZ_LISTEN" => Some("nowhere".into()),
            _ => None,
        });
        assert!(bad_listen.is_err());
    }
}
