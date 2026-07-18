//! Non-secret service configuration.

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

use spurfire_protocol::{ProvisioningMode, MAX_PLAYERS};
use thiserror::Error;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_SHARED_TAILNET: &str = "-";
const DEFAULT_STATE_PATH: &str = ".spurfire/server-state.json";
const DEFAULT_DRY_RUN_TTL_SECS: u64 = 5 * 60;

/// HTTP service configuration. OAuth values deliberately do not live here.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// Socket address used by the binary.
    pub bind_addr: SocketAddr,
    /// Dry-run lobby TTL, hard-capped at five minutes.
    pub dry_run_ttl: Duration,
    /// Deployment-level roster cap, bounded by the protocol hard cap.
    pub max_players: u8,
    /// Deployment provisioning mode.
    pub provisioning_mode: ProvisioningMode,
    /// Explicit service-wide simulation switch.
    pub force_dry_run: bool,
    /// Independent fail-closed gate for every real provider mutation.
    ///
    /// Credentials and a real provisioning mode are insufficient without this
    /// switch. It defaults off and hosted public deployments must keep it off.
    pub real_mutations_enabled: bool,
    /// Separate product admission gate; provider capability probes never open Create.
    pub real_admission_enabled: bool,
    /// Explicit development-only compatibility for legacy asserted-player mutations.
    pub allow_legacy_client_assertions: bool,
    /// Trust `X-Forwarded-For` only when the deployment has a known gateway boundary.
    pub trust_forwarded_for: bool,
    /// Encrypted dynamic child-vault file (required when real mutations are enabled).
    pub child_vault_path: PathBuf,
    /// File containing exactly 32 raw bytes or 64 hex characters; never an env value.
    pub child_vault_key_path: PathBuf,
    /// Shared tailnet selector passed to Tailscale (`-` means the token's tailnet).
    pub shared_tailnet: String,
    /// Durable non-secret state path used in real mode.
    pub state_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR
                .parse()
                .expect("the built-in bind address is valid"),
            dry_run_ttl: Duration::from_secs(DEFAULT_DRY_RUN_TTL_SECS),
            max_players: MAX_PLAYERS,
            provisioning_mode: ProvisioningMode::SharedTailnet,
            force_dry_run: false,
            real_mutations_enabled: false,
            real_admission_enabled: false,
            allow_legacy_client_assertions: false,
            trust_forwarded_for: false,
            child_vault_path: PathBuf::from(".spurfire/child-vault.json"),
            child_vault_key_path: PathBuf::from(".spurfire/child-vault.key"),
            shared_tailnet: DEFAULT_SHARED_TAILNET.to_owned(),
            state_path: PathBuf::from(DEFAULT_STATE_PATH),
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("dry_run_ttl", &self.dry_run_ttl)
            .field("max_players", &self.max_players)
            .field("provisioning_mode", &self.provisioning_mode)
            .field("force_dry_run", &self.force_dry_run)
            .field("real_mutations_enabled", &self.real_mutations_enabled)
            .field("real_admission_enabled", &self.real_admission_enabled)
            .field(
                "allow_legacy_client_assertions",
                &self.allow_legacy_client_assertions,
            )
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .field("child_vault_path", &self.child_vault_path)
            .field("child_vault_key_path", &"<configured-key-file>")
            .field("shared_tailnet", &"<configured>")
            .field("state_path", &self.state_path)
            .finish()
    }
}

impl Config {
    /// Loads non-secret settings from the process environment.
    ///
    /// Supported variables are `SPURFIRE_BIND_ADDR`,
    /// `SPURFIRE_DRY_RUN_TTL_SECS`, `SPURFIRE_MAX_PLAYERS`,
    /// `SPURFIRE_PROVISIONING_MODE`, `SPURFIRE_SHARED_TAILNET`,
    /// `SPURFIRE_STATE_PATH`, `SPURFIRE_DRY_RUN`, and
    /// `SPURFIRE_REAL_MUTATIONS_ENABLED`.
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut config = Self::default();

        if let Some(value) = env_value("SPURFIRE_BIND_ADDR")? {
            config.bind_addr = value.parse().map_err(|_| ConfigError::InvalidBindAddress)?;
        }
        if let Some(value) = env_value("SPURFIRE_DRY_RUN_TTL_SECS")? {
            let seconds = value
                .parse::<u64>()
                .map_err(|_| ConfigError::InvalidDryRunTtl)?;
            if seconds == 0 || seconds > DEFAULT_DRY_RUN_TTL_SECS {
                return Err(ConfigError::InvalidDryRunTtl);
            }
            config.dry_run_ttl = Duration::from_secs(seconds);
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
        if let Some(value) = env_value("SPURFIRE_STATE_PATH")? {
            if value.trim().is_empty() {
                return Err(ConfigError::InvalidStatePath);
            }
            config.state_path = PathBuf::from(value);
        }
        if let Some(value) = env_value("SPURFIRE_DRY_RUN")? {
            config.force_dry_run = parse_binary_switch(&value, ConfigError::InvalidDryRun)?;
        }
        if let Some(value) = env_value("SPURFIRE_REAL_MUTATIONS_ENABLED")? {
            config.real_mutations_enabled =
                parse_binary_switch(&value, ConfigError::InvalidRealMutationsSwitch)?;
        }
        if let Some(value) = env_value("SPURFIRE_REAL_ADMISSION_ENABLED")? {
            config.real_admission_enabled =
                parse_binary_switch(&value, ConfigError::InvalidRealAdmissionSwitch)?;
        }
        if let Some(value) = env_value("SPURFIRE_ALLOW_LEGACY_CLIENT_ASSERTIONS")? {
            config.allow_legacy_client_assertions =
                parse_binary_switch(&value, ConfigError::InvalidLegacyAssertionsSwitch)?;
        }
        if let Some(value) = env_value("SPURFIRE_TRUST_FORWARDED_FOR")? {
            config.trust_forwarded_for =
                parse_binary_switch(&value, ConfigError::InvalidTrustedProxySwitch)?;
        }
        if let Some(value) = env_value("SPURFIRE_CHILD_VAULT_PATH")? {
            if value.trim().is_empty() {
                return Err(ConfigError::InvalidChildVaultPath);
            }
            config.child_vault_path = PathBuf::from(value);
        }
        if let Some(value) = env_value("SPURFIRE_CHILD_VAULT_KEY_FILE")? {
            if value.trim().is_empty() {
                return Err(ConfigError::InvalidChildVaultKeyPath);
            }
            config.child_vault_key_path = PathBuf::from(value);
        }

        if config.real_admission_enabled && !config.real_mutations_enabled {
            return Err(ConfigError::AdmissionRequiresMutationGate);
        }
        if config.allow_legacy_client_assertions && config.real_admission_enabled {
            return Err(ConfigError::LegacyAssertionsConflictWithAdmission);
        }
        if config.force_dry_run && config.real_mutations_enabled {
            return Err(ConfigError::ConflictingMutationSwitches);
        }
        if config.provisioning_mode == ProvisioningMode::DryRun && !config.force_dry_run {
            return Err(ConfigError::DryRunRequiresOptIn);
        }
        if config.force_dry_run {
            config.provisioning_mode = ProvisioningMode::DryRun;
            config.real_mutations_enabled = false;
            config.real_admission_enabled = false;
        }
        Ok(config)
    }

    /// Returns the configured dry-run TTL in milliseconds.
    #[must_use]
    pub fn dry_run_ttl_ms(&self) -> u64 {
        u64::try_from(self.dry_run_ttl.as_millis()).unwrap_or(5 * 60 * 1_000)
    }
}

fn env_value(name: &'static str) -> Result<Option<String>, ConfigError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ConfigError::NonUnicode(name)),
    }
}

fn parse_binary_switch(value: &str, error: ConfigError) -> Result<bool, ConfigError> {
    match value {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(error),
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
    /// Dry-run TTL was outside 1..=300 seconds.
    #[error("SPURFIRE_DRY_RUN_TTL_SECS must be between 1 and 300")]
    InvalidDryRunTtl,
    /// Deployment cap violated the protocol bound.
    #[error("SPURFIRE_MAX_PLAYERS must be between 1 and {MAX_PLAYERS}")]
    InvalidMaxPlayers,
    /// Provisioning mode was unknown.
    #[error("SPURFIRE_PROVISIONING_MODE must be shared_tailnet, tailnet_per_lobby, or dry_run")]
    InvalidProvisioningMode,
    /// Dry-run mode was selected without the explicit switch.
    #[error("dry_run service mode requires SPURFIRE_DRY_RUN=1")]
    DryRunRequiresOptIn,
    /// Dry-run switch was neither zero nor one.
    #[error("SPURFIRE_DRY_RUN must be 0 or 1")]
    InvalidDryRun,
    /// Real-mutation switch was neither zero nor one.
    #[error("SPURFIRE_REAL_MUTATIONS_ENABLED must be 0 or 1")]
    InvalidRealMutationsSwitch,
    /// Product admission switch was invalid.
    #[error("SPURFIRE_REAL_ADMISSION_ENABLED must be 0 or 1")]
    InvalidRealAdmissionSwitch,
    /// Legacy assertion switch was invalid.
    #[error("SPURFIRE_ALLOW_LEGACY_CLIENT_ASSERTIONS must be 0 or 1")]
    InvalidLegacyAssertionsSwitch,
    /// Trusted proxy switch was invalid.
    #[error("SPURFIRE_TRUST_FORWARDED_FOR must be 0 or 1")]
    InvalidTrustedProxySwitch,
    /// Admission cannot open while provider mutation remains disabled.
    #[error("real admission requires SPURFIRE_REAL_MUTATIONS_ENABLED=1")]
    AdmissionRequiresMutationGate,
    /// Legacy assertions are forbidden whenever real admission is open.
    #[error("legacy client assertions conflict with real admission")]
    LegacyAssertionsConflictWithAdmission,
    /// Child vault path was empty.
    #[error("SPURFIRE_CHILD_VAULT_PATH must not be empty")]
    InvalidChildVaultPath,
    /// Child vault key path was empty.
    #[error("SPURFIRE_CHILD_VAULT_KEY_FILE must not be empty")]
    InvalidChildVaultKeyPath,
    /// Simulation and real mutation cannot both be requested.
    #[error("SPURFIRE_DRY_RUN=1 conflicts with SPURFIRE_REAL_MUTATIONS_ENABLED=1")]
    ConflictingMutationSwitches,
    /// Shared tailnet selector was empty.
    #[error("SPURFIRE_SHARED_TAILNET must not be empty")]
    InvalidSharedTailnet,
    /// Durable state path was empty.
    #[error("SPURFIRE_STATE_PATH must not be empty")]
    InvalidStatePath,
}
