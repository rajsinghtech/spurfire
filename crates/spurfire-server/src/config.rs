//! Non-secret service configuration.

use std::{fmt, net::SocketAddr, time::Duration};

use spurfire_protocol::{ProvisioningMode, ABSOLUTE_TTL_MS, MAX_PLAYERS};
use thiserror::Error;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_SHARED_TAILNET: &str = "-";

/// HTTP service configuration. OAuth values deliberately do not live here.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// Socket address used by the binary.
    pub bind_addr: SocketAddr,
    /// Fixed lobby absolute TTL (60 minutes by default).
    pub default_ttl: Duration,
    /// Deployment-level roster cap, bounded by the protocol hard cap.
    pub max_players: u8,
    /// Deployment provisioning mode.
    pub provisioning_mode: ProvisioningMode,
    /// Explicit service-wide simulation switch.
    pub force_dry_run: bool,
    /// Shared tailnet selector passed to Tailscale (`-` means the token's tailnet).
    pub shared_tailnet: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR
                .parse()
                .expect("the built-in bind address is valid"),
            default_ttl: Duration::from_millis(ABSOLUTE_TTL_MS),
            max_players: MAX_PLAYERS,
            provisioning_mode: ProvisioningMode::SharedTailnet,
            force_dry_run: false,
            shared_tailnet: DEFAULT_SHARED_TAILNET.to_owned(),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("default_ttl", &self.default_ttl)
            .field("max_players", &self.max_players)
            .field("provisioning_mode", &self.provisioning_mode)
            .field("force_dry_run", &self.force_dry_run)
            .field("shared_tailnet", &"<configured>")
            .finish()
    }
}

impl Config {
    /// Loads non-secret settings from the process environment.
    ///
    /// Supported variables are `SPURFIRE_BIND_ADDR`,
    /// `SPURFIRE_DEFAULT_TTL_SECS`, `SPURFIRE_MAX_PLAYERS`,
    /// `SPURFIRE_PROVISIONING_MODE`, `SPURFIRE_SHARED_TAILNET`, and
    /// `SPURFIRE_DRY_RUN`. Simulation is service-wide only when the last value
    /// is exactly `1`.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut config = Self::default();

        if let Some(value) = env_value("SPURFIRE_BIND_ADDR")? {
            config.bind_addr = value.parse().map_err(|_| ConfigError::InvalidBindAddress)?;
        }
        if let Some(value) = env_value("SPURFIRE_DEFAULT_TTL_SECS")? {
            let seconds = value
                .parse::<u64>()
                .map_err(|_| ConfigError::InvalidDefaultTtl)?;
            if seconds == 0 {
                return Err(ConfigError::InvalidDefaultTtl);
            }
            config.default_ttl = Duration::from_secs(seconds);
        }
        if let Some(value) = env_value("SPURFIRE_MAX_PLAYERS")? {
            let players = value
                .parse::<u8>()
                .map_err(|_| ConfigError::InvalidMaxPlayers)?;
            if players == 0 || players > MAX_PLAYERS {
                return Err(ConfigError::InvalidMaxPlayers);
            }
            config.max_players = players;
        }
        if let Some(value) = env_value("SPURFIRE_PROVISIONING_MODE")? {
            config.provisioning_mode = parse_mode(&value)?;
        }
        if let Some(value) = env_value("SPURFIRE_SHARED_TAILNET")? {
            if value.trim().is_empty() {
                return Err(ConfigError::InvalidSharedTailnet);
            }
            config.shared_tailnet = value;
        }
        if let Some(value) = env_value("SPURFIRE_DRY_RUN")? {
            config.force_dry_run = match value.as_str() {
                "1" => true,
                "0" => false,
                _ => return Err(ConfigError::InvalidDryRun),
            };
        }

        if config.provisioning_mode == ProvisioningMode::TailnetPerLobby {
            return Err(ConfigError::ModeUnavailable);
        }
        if config.provisioning_mode == ProvisioningMode::DryRun && !config.force_dry_run {
            return Err(ConfigError::DryRunRequiresOptIn);
        }
        if config.force_dry_run {
            config.provisioning_mode = ProvisioningMode::DryRun;
        }
        Ok(config)
    }

    /// Returns the configured TTL in milliseconds, saturating for extreme input.
    #[must_use]
    pub fn default_ttl_ms(&self) -> u64 {
        u64::try_from(self.default_ttl.as_millis()).unwrap_or(u64::MAX)
    }
}

fn env_value(name: &'static str) -> Result<Option<String>, ConfigError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::NonUnicode(name)),
    }
}

fn parse_mode(value: &str) -> Result<ProvisioningMode, ConfigError> {
    match value {
        "shared_tailnet" => Ok(ProvisioningMode::SharedTailnet),
        "dry_run" => Ok(ProvisioningMode::DryRun),
        "tailnet_per_lobby" => Ok(ProvisioningMode::TailnetPerLobby),
        _ => Err(ConfigError::InvalidProvisioningMode),
    }
}

/// Invalid non-secret service configuration.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// A configured value was not valid Unicode.
    #[error("environment variable {0} is not valid Unicode")]
    NonUnicode(&'static str),
    /// Bind address could not be parsed.
    #[error("SPURFIRE_BIND_ADDR must be a socket address")]
    InvalidBindAddress,
    /// TTL was absent, zero, or not an integer.
    #[error("SPURFIRE_DEFAULT_TTL_SECS must be a positive integer")]
    InvalidDefaultTtl,
    /// Deployment cap violated the protocol bound.
    #[error("SPURFIRE_MAX_PLAYERS must be between 1 and {MAX_PLAYERS}")]
    InvalidMaxPlayers,
    /// Provisioning mode was unknown.
    #[error("SPURFIRE_PROVISIONING_MODE must be shared_tailnet or dry_run")]
    InvalidProvisioningMode,
    /// Tailnet-per-lobby remains unavailable.
    #[error("tailnet_per_lobby is unavailable because tested create routes returned 404")]
    ModeUnavailable,
    /// Dry-run mode was selected without the explicit switch.
    #[error("dry_run service mode requires SPURFIRE_DRY_RUN=1")]
    DryRunRequiresOptIn,
    /// Dry-run switch was neither zero nor one.
    #[error("SPURFIRE_DRY_RUN must be 0 or 1")]
    InvalidDryRun,
    /// Shared tailnet selector was empty.
    #[error("SPURFIRE_SHARED_TAILNET must not be empty")]
    InvalidSharedTailnet,
}
