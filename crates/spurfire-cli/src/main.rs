//! `spurfire-ctl` — lobby lifecycle CLI.
//!
//! Lobby metadata is persisted in the user's data directory. Auth-key values are never
//! persisted; newly minted credentials are displayed once by `lobby create`.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use spurfire_control::{AuthKeyOpts, ProvisioningMode, TailscaleClient};

const SHARED_TAILNET: &str = "-";
const DEFAULT_PLAYERS: u8 = 8;

#[derive(Debug, Parser)]
#[command(name = "spurfire-ctl", about = "Manage Spurfire lobbies")]
struct Cli {
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage lobby lifecycle.
    Lobby {
        #[command(subcommand)]
        command: LobbyCommand,
    },
}

#[derive(Debug, Subcommand)]
enum LobbyCommand {
    /// Provision a lobby and mint one credential per player slot.
    Create(CreateArgs),
    /// List locally tracked lobbies.
    List,
    /// Show a lobby and its currently connected devices.
    Status(NameArgs),
    /// Remove lobby devices/container and local metadata.
    Destroy(NameArgs),
}

#[derive(Debug, Args)]
struct CreateArgs {
    #[arg(long)]
    name: String,
    #[arg(long, default_value_t = DEFAULT_PLAYERS, value_parser = clap::value_parser!(u8).range(1..=16))]
    players: u8,
    #[arg(long, value_enum, default_value_t = ModeArg::Shared)]
    mode: ModeArg,
}

#[derive(Debug, Args)]
struct NameArgs {
    #[arg(long)]
    name: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    #[value(name = "tailnet-per-lobby")]
    TailnetPerLobby,
    Shared,
}

impl From<ModeArg> for ProvisioningMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::TailnetPerLobby => Self::TailnetPerLobby,
            ModeArg::Shared => Self::SharedTailnet,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Lobby {
    name: String,
    players: u8,
    mode: ProvisioningMode,
    tailnet: String,
    tag: String,
    created_unix_secs: u64,
    #[serde(default)]
    auth_key_ids: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LobbyFile {
    #[serde(default)]
    lobbies: Vec<Lobby>,
}

struct Store {
    path: PathBuf,
    data: LobbyFile,
}

impl Store {
    fn load() -> Result<Self> {
        let path = data_path()?;
        Self::load_at(path)
    }

    fn load_at(path: PathBuf) -> Result<Self> {
        let data = if path.exists() {
            let bytes = fs::read(&path)
                .with_context(|| format!("failed to read lobby metadata at {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse lobby metadata at {}", path.display()))?
        } else {
            LobbyFile::default()
        };
        Ok(Self { path, data })
    }

    fn save(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("lobby metadata path has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let temporary = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.data)?;
        fs::write(&temporary, bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        Ok(())
    }

    fn get(&self, name: &str) -> Result<&Lobby> {
        self.data
            .lobbies
            .iter()
            .find(|lobby| lobby.name == name)
            .ok_or_else(|| anyhow!("lobby {name:?} is not tracked locally"))
    }
}

#[derive(Serialize)]
struct CreateOutput<'a> {
    lobby: &'a Lobby,
    /// Secret credentials, emitted once and never persisted.
    auth_keys: Vec<&'a str>,
}

#[derive(Serialize)]
struct StatusOutput<'a> {
    lobby: &'a Lobby,
    devices: Vec<spurfire_control::Device>,
}

#[derive(Serialize)]
struct DestroyOutput<'a> {
    name: &'a str,
    deleted_devices: usize,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    // dotenv() searches cwd and its parents. An absent file is fine when env is set directly.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match cli.command {
        Command::Lobby { command } => match command {
            LobbyCommand::Create(args) => create(args, cli.json).await,
            LobbyCommand::List => list(cli.json),
            LobbyCommand::Status(args) => status(&args.name, cli.json).await,
            LobbyCommand::Destroy(args) => destroy(&args.name, cli.json).await,
        },
    }
}

async fn create(args: CreateArgs, json: bool) -> Result<()> {
    let name = args.name.trim();
    if name.is_empty() {
        bail!("lobby name must not be empty");
    }

    let mut store = Store::load()?;
    if store.data.lobbies.iter().any(|lobby| lobby.name == name) {
        bail!("lobby {name:?} is already tracked locally");
    }

    let mode = ProvisioningMode::from(args.mode);
    if mode == ProvisioningMode::TailnetPerLobby {
        bail!(
            "tailnet-per-lobby is server-only: spurfire-ctl will not persist child OAuth secrets; use spurfire-server"
        );
    }
    let client = TailscaleClient::from_env().await?;
    let tailnet = SHARED_TAILNET.to_owned();

    let tag = lobby_tag(name);
    let lobby = Lobby {
        name: name.to_owned(),
        players: args.players,
        mode,
        tailnet,
        tag: tag.clone(),
        created_unix_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs(),
        auth_key_ids: Vec::new(),
    };

    // Persist before minting so even an interrupted/partially failed create remains cleanable.
    store.data.lobbies.push(lobby);
    store.save()?;
    let lobby_index = store.data.lobbies.len() - 1;
    let opts = AuthKeyOpts {
        tags: vec![tag],
        ..AuthKeyOpts::default()
    };
    let mut auth_keys = Vec::with_capacity(usize::from(args.players));
    for _ in 0..args.players {
        let key = client
            .create_auth_key(&store.data.lobbies[lobby_index].tailnet, &opts)
            .await?;
        store.data.lobbies[lobby_index].auth_key_ids.push(key.id);
        store.save()?;
        auth_keys.push(key.key);
    }

    let lobby = &store.data.lobbies[lobby_index];
    if json {
        print_json(&CreateOutput {
            lobby,
            auth_keys: auth_keys.iter().map(String::as_str).collect(),
        })?;
    } else {
        println!(
            "created lobby {:?} in {:?} mode ({} player slots)",
            lobby.name, lobby.mode, lobby.players
        );
        println!("credentials (shown once):");
        for (index, key) in auth_keys.iter().enumerate() {
            println!("  player {}: {key}", index + 1);
        }
    }
    Ok(())
}

fn list(json: bool) -> Result<()> {
    let store = Store::load()?;
    if json {
        print_json(&store.data.lobbies)
    } else if store.data.lobbies.is_empty() {
        println!("no lobbies tracked");
        Ok(())
    } else {
        for lobby in &store.data.lobbies {
            println!(
                "{}\t{:?}\t{} players\t{}",
                lobby.name, lobby.mode, lobby.players, lobby.tailnet
            );
        }
        Ok(())
    }
}

async fn status(name: &str, json: bool) -> Result<()> {
    let store = Store::load()?;
    let lobby = store.get(name)?;
    if lobby.mode == ProvisioningMode::TailnetPerLobby {
        bail!(
            "tailnet-per-lobby status requires the server's in-memory child OAuth vault; no secret is stored by spurfire-ctl"
        );
    }
    let client = TailscaleClient::from_env().await?;
    let devices = client
        .list_devices(&lobby.tailnet)
        .await?
        .into_iter()
        .filter(|device| {
            lobby.mode == ProvisioningMode::TailnetPerLobby
                || device.tags.iter().any(|tag| tag == &lobby.tag)
        })
        .collect::<Vec<_>>();
    if json {
        print_json(&StatusOutput { lobby, devices })
    } else {
        println!(
            "lobby {:?}: {:?}, {}/{} connected",
            lobby.name,
            lobby.mode,
            devices.len(),
            lobby.players
        );
        for device in devices {
            println!("  {}\t{}", device.name, device.addresses.join(","));
        }
        Ok(())
    }
}

async fn destroy(name: &str, json: bool) -> Result<()> {
    let mut store = Store::load()?;
    let position = store
        .data
        .lobbies
        .iter()
        .position(|lobby| lobby.name == name)
        .ok_or_else(|| anyhow!("lobby {name:?} is not tracked locally"))?;
    let lobby = store.data.lobbies[position].clone();
    let client = TailscaleClient::from_env().await?;
    let deleted_devices = match lobby.mode {
        ProvisioningMode::TailnetPerLobby => {
            bail!(
                "tailnet-per-lobby cleanup requires manual remediation because spurfire-ctl never persisted the child OAuth secret"
            );
        }
        ProvisioningMode::SharedTailnet => {
            let devices = client.list_devices(&lobby.tailnet).await?;
            let matching = devices
                .into_iter()
                .filter(|device| device.tags.iter().any(|tag| tag == &lobby.tag))
                .collect::<Vec<_>>();
            for device in &matching {
                client.delete_device(&device.id).await?;
            }
            matching.len()
        }
    };

    store.data.lobbies.remove(position);
    store.save()?;
    if json {
        print_json(&DestroyOutput {
            name,
            deleted_devices,
        })
    } else {
        println!("destroyed lobby {name:?} ({deleted_devices} devices removed)");
        Ok(())
    }
}

fn lobby_tag(name: &str) -> String {
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "tag:spurfire-lobby".to_owned()
    } else {
        format!("tag:spurfire-{slug}")
    }
}

fn data_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(Path::new(&path).join("spurfire/lobbies.json"));
    }
    let home = env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(Path::new(&home).join(".local/share/spurfire/lobbies.json"))
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_stable_lobby_tags() {
        assert_eq!(lobby_tag("High Noon!"), "tag:spurfire-high-noon");
        assert_eq!(lobby_tag("!!!"), "tag:spurfire-lobby");
    }

    #[test]
    fn store_round_trips_without_secrets() {
        let root = env::temp_dir().join(format!(
            "spurfire-cli-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("lobbies.json");
        let mut store = Store::load_at(path.clone()).unwrap();
        store.data.lobbies.push(Lobby {
            name: "test".into(),
            players: 2,
            mode: ProvisioningMode::SharedTailnet,
            tailnet: "-".into(),
            tag: "tag:spurfire-test".into(),
            created_unix_secs: 1,
            auth_key_ids: vec!["key-id".into()],
        });
        store.save().unwrap();

        let loaded = Store::load_at(path).unwrap();
        assert_eq!(loaded.get("test").unwrap().auth_key_ids, ["key-id"]);
        fs::remove_dir_all(root).unwrap();
    }
}
