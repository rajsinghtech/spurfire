//! `spurfire-ctl` — lobby lifecycle and selected-lobby inspection CLI.
//!
//! Lobby metadata is persisted in the user's data directory. Auth-key values are never
//! persisted; newly minted credentials are displayed once by `lobby create`. Inspection
//! capabilities are read from a file or stdin, sent only over HTTPS, and never persisted.

use std::{
    env, fmt,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::{
    header::{HeaderValue, ACCEPT, AUTHORIZATION, CACHE_CONTROL},
    redirect::Policy,
    Client, Request, Response, Url,
};
use serde::{Deserialize, Serialize};
use spurfire_control::{AuthKeyOpts, ProvisioningMode, TailscaleClient};
use spurfire_protocol::{Fact, LobbyId, LobbyNetworkView, UnixMillis};
use zeroize::Zeroizing;

const SHARED_TAILNET: &str = "-";
const DEFAULT_PLAYERS: u8 = 8;
const CAPABILITY_SCHEME: &str = "Spurfire-Capability";
const MIN_CAPABILITY_CHARS: usize = 43;
const MAX_CAPABILITY_BYTES: usize = 1_024;
const MAX_INSPECTION_RESPONSE_BYTES: usize = 1024 * 1024;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(10);
const INSPECTION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Inspect one exact capability-authorized lobby network.
    Inspect(InspectArgs),
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
struct InspectArgs {
    /// Exact lobby UUID. An identifier alone grants no access.
    #[arg(long, value_name = "UUID")]
    lobby: LobbyId,
    /// Spurfire control-service base URL. HTTPS is required.
    #[arg(long, value_name = "HTTPS_URL")]
    server: String,
    /// File containing the capability, or `-` to read it from stdin.
    #[arg(long, value_name = "PATH|-", verbatim_doc_comment)]
    cap_file: PathBuf,
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
            LobbyCommand::Inspect(args) => inspect(args, cli.json).await,
            LobbyCommand::List => list(cli.json),
            LobbyCommand::Status(args) => status(&args.name, cli.json).await,
            LobbyCommand::Destroy(args) => destroy(&args.name, cli.json).await,
        },
    }
}

struct SecretCapability(Zeroizing<String>);

impl SecretCapability {
    fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for SecretCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretCapability(<redacted>)")
    }
}

async fn inspect(args: InspectArgs, json: bool) -> Result<()> {
    let capability = read_capability(&args.cap_file)?;
    let endpoint = inspection_endpoint(&args.server, args.lobby)?;
    let client = inspection_client()?;
    let request = build_inspection_request(&client, endpoint, &capability)?;
    let response = client
        .execute(request)
        .await
        .context("selected-lobby inspection request failed")?;
    let view = decode_inspection_response(response).await?;

    if view.lobby_id != args.lobby {
        bail!("inspection response did not match the selected lobby");
    }
    view.validate()
        .map_err(|_| anyhow!("inspection response failed schema validation"))?;

    if json {
        print_json(&view)
    } else {
        print!("{}", render_inspection(&view));
        Ok(())
    }
}

fn read_capability(path: &Path) -> Result<SecretCapability> {
    if path == Path::new("-") {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        return read_capability_from(&mut lock).context("failed to read capability from stdin");
    }

    let mut file = fs::File::open(path).context("failed to open capability file")?;
    read_capability_from(&mut file).context("failed to read capability file")
}

fn read_capability_from(reader: &mut impl Read) -> Result<SecretCapability> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(128));
    reader
        .take((MAX_CAPABILITY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("capability input could not be read")?;
    if bytes.len() > MAX_CAPABILITY_BYTES {
        bail!("capability input exceeds the safe size limit");
    }

    let value = std::str::from_utf8(bytes.as_slice())
        .map_err(|_| anyhow!("capability input must be base64url text"))?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.len() < MIN_CAPABILITY_CHARS
        || value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("capability input must contain exactly one base64url capability");
    }

    Ok(SecretCapability(Zeroizing::new(value.to_owned())))
}

fn inspection_client() -> Result<Client> {
    Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .referer(false)
        .connect_timeout(INSPECTION_CONNECT_TIMEOUT)
        .timeout(INSPECTION_TIMEOUT)
        .user_agent(concat!("spurfire-ctl/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to initialize the HTTPS inspection client")
}

fn inspection_endpoint(server: &str, lobby_id: LobbyId) -> Result<Url> {
    let mut endpoint = Url::parse(server).context("--server must be a valid HTTPS URL")?;
    if endpoint.scheme() != "https" {
        bail!("--server must use HTTPS; capabilities are never sent over cleartext HTTP");
    }
    if endpoint.host_str().is_none() {
        bail!("--server must include a host");
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        bail!("--server must not contain user information");
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        bail!("--server must not contain a query or fragment");
    }

    let lobby_id = lobby_id.to_string();
    let mut segments = endpoint
        .path_segments_mut()
        .map_err(|_| anyhow!("--server cannot be used as a hierarchical base URL"))?;
    segments.pop_if_empty();
    segments.extend(["v1", "lobbies", lobby_id.as_str(), "network"]);
    drop(segments);
    Ok(endpoint)
}

fn build_inspection_request(
    client: &Client,
    endpoint: Url,
    capability: &SecretCapability,
) -> Result<Request> {
    let mut authorization = Zeroizing::new(String::with_capacity(
        CAPABILITY_SCHEME.len() + 1 + capability.expose_secret().len(),
    ));
    authorization.push_str(CAPABILITY_SCHEME);
    authorization.push(' ');
    authorization.push_str(capability.expose_secret());
    let mut header = HeaderValue::from_str(authorization.as_str())
        .map_err(|_| anyhow!("capability could not be represented as an HTTP header"))?;
    header.set_sensitive(true);

    client
        .get(endpoint)
        .header(AUTHORIZATION, header)
        .header(ACCEPT, "application/json")
        .header(CACHE_CONTROL, "no-store")
        .build()
        .context("failed to build the selected-lobby inspection request")
}

async fn decode_inspection_response(mut response: Response) -> Result<LobbyNetworkView> {
    let status = response.status();
    let body = read_bounded_response(&mut response).await?;
    if !status.is_success() {
        let code = safe_error_code(&body);
        if let Some(code) = code {
            bail!(
                "selected lobby is unavailable or unauthorized (HTTP {}, {code})",
                status.as_u16()
            );
        }
        bail!(
            "selected lobby is unavailable or unauthorized (HTTP {})",
            status.as_u16()
        );
    }

    serde_json::from_slice(&body)
        .map_err(|_| anyhow!("inspection response was not a valid lobby network view"))
}

async fn read_bounded_response(response: &mut Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSPECTION_RESPONSE_BYTES as u64)
    {
        bail!("inspection response exceeds the safe size limit");
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("inspection response body could not be read")?
    {
        let new_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("inspection response exceeds the safe size limit"))?;
        if new_len > MAX_INSPECTION_RESPONSE_BYTES {
            bail!("inspection response exceeds the safe size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn safe_error_code(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorCode {
        code: String,
    }

    let code = serde_json::from_slice::<ErrorCode>(body).ok()?.code;
    (code.len() <= 64
        && !code.is_empty()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(code)
}

fn render_inspection(view: &LobbyNetworkView) -> String {
    let mut output = String::new();
    macro_rules! line {
        ($($argument:tt)*) => {
            writeln!(&mut output, $($argument)*).expect("writing to a String cannot fail")
        };
    }

    line!("SPURFIRE LOBBY NETWORK");
    line!("Truth: {}", wire_value(&view.truth_label));
    line!("Lobby: {}", view.lobby_id);
    line!("Schema: {}", view.schema_version);
    line!("Served at: {}", format_unix_millis(view.served_at));
    line!();

    line!("BACKING (cached control state)");
    line!("  Mode: {}", wire_value(&view.backing.backing_mode));
    line!(
        "  Simulates mode: {}",
        view.backing
            .simulates_mode
            .as_ref()
            .map_or_else(|| "not applicable".to_owned(), wire_value)
    );
    line!("  Isolation: {}", wire_value(&view.backing.isolation));
    line!("  Network generation: {}", view.backing.network_generation);
    line!(
        "  Network lifecycle: {}",
        wire_value(&view.backing.network_lifecycle)
    );
    line!(
        "  Lobby lifecycle: {}",
        describe_fact(&view.lobby_lifecycle, wire_value)
    );
    line!(
        "  Tailnet DNS name / FQDN: {}",
        describe_fact(&view.backing.tailnet_dns_name, |value| value
            .as_str()
            .to_owned())
    );
    line!(
        "  Control service tailnet member: {}",
        describe_fact(&view.backing.control_service_member, |value| if *value {
            "yes".to_owned()
        } else {
            "no — lifecycle owner only; not joined".to_owned()
        })
    );
    line!();

    line!("PEERS AND REPORT FRESHNESS");
    line!(
        "  Roster participants: {}",
        describe_fact(&view.counts.roster_count, ToString::to_string)
    );
    line!(
        "  Provider-enrolled devices: {}",
        describe_fact(
            &view.counts.provider_enrolled_device_count,
            ToString::to_string
        )
    );
    line!(
        "  Provider-online devices: {}",
        describe_fact(
            &view.counts.provider_online_device_count,
            ToString::to_string
        )
    );
    line!(
        "  Fresh participant reporters: {}",
        describe_fact(&view.counts.fresh_reporter_count, ToString::to_string)
    );
    line!(
        "  Fresh directional observations: {}",
        describe_fact(
            &view.counts.fresh_directional_observation_count,
            ToString::to_string
        )
    );
    line!();

    line!("DIRECTIONAL ROUTES (reverse paths are never inferred)");
    line!(
        "  Expected directions: {}",
        describe_fact(&view.routes.expected_direction_count, ToString::to_string)
    );
    line!(
        "  Reported directions: {}",
        describe_fact(&view.routes.reported_direction_count, ToString::to_string)
    );
    line!(
        "  Direct: {}",
        describe_fact(&view.routes.direct_count, ToString::to_string)
    );
    line!(
        "  Peer Relay: {}",
        describe_fact(&view.routes.peer_relay_count, ToString::to_string)
    );
    line!(
        "  DERP Relay: {}",
        describe_fact(&view.routes.derp_relay_count, ToString::to_string)
    );
    line!(
        "  Unavailable: {}",
        describe_fact(&view.routes.unavailable_count, ToString::to_string)
    );
    line!(
        "  Unknown: {}",
        describe_fact(&view.routes.unknown_count, ToString::to_string)
    );
    line!(
        "  Reachable known: {}",
        describe_fact(&view.routes.reachable_known_count, ToString::to_string)
    );
    line!(
        "  Direct ratio: {}",
        describe_fact(&view.routes.direct_ratio_milli, |value| format!(
            "{:.1}% ({value}/1000)",
            f64::from(*value) / 10.0
        ))
    );
    line!();

    line!("APPLICATION QUALITY (nonce/reply measurements only)");
    line!(
        "  Samples: {}",
        describe_fact(&view.application_quality.sample_count, ToString::to_string)
    );
    line!(
        "  RTT median: {}",
        describe_fact(
            &view.application_quality.application_rtt_ms_median,
            |value| format!("{value} ms")
        )
    );
    line!(
        "  RTT p95: {}",
        describe_fact(
            &view.application_quality.application_rtt_ms_p95,
            |value| format!("{value} ms")
        )
    );
    line!(
        "  RTT worst: {}",
        describe_fact(
            &view.application_quality.application_rtt_ms_worst,
            |value| format!("{value} ms")
        )
    );
    line!(
        "  Loss median: {}",
        describe_fact(
            &view.application_quality.application_loss_ppm_median,
            |value| format!("{:.4}% ({value} ppm)", f64::from(*value) / 10_000.0)
        )
    );
    line!();

    line!("AUTHORITY (inspection never changes gameplay authority)");
    line!(
        "  Deterministic control election: {}",
        describe_fact(&view.authority.control_election, |value| {
            format!(
            "winner {}; score {}; formula {}; input {}; evaluated {}; input assurance {}; degraded {}",
            value.winner_player_id,
            value.score_milli,
            value.formula_version,
            value.input_hash,
            format_unix_millis(value.evaluated_at),
            wire_value(&value.input_assurance),
            value.degraded
        )
        })
    );
    line!(
        "  Last accepted heartbeat receipt: {}",
        describe_fact(&view.authority.last_accepted_heartbeat, |value| format!(
            "player {}; epoch {}; input {}; received {}",
            value.player_id,
            value.epoch,
            value.input_hash,
            format_unix_millis(value.received_at)
        ))
    );
    line!(
        "  Peer-reported match authority: {}",
        describe_fact(&view.authority.peer_reported_match_authority, |value| {
            format!(
                "player {}; epoch {}; input {}; reporters {}; agree {}; conflict {} (reported, not ranked proof)",
                value.player_id,
                value.epoch,
                value.input_hash,
                value.fresh_reporter_count,
                value.agreement_count,
                value.conflict_count
            )
        })
    );
    line!();

    line!("CLEANUP");
    line!(
        "  Network lifecycle: {}",
        wire_value(&view.cleanup.network_lifecycle)
    );
    line!(
        "  Participant-safe reason: {}",
        wire_value(&view.cleanup.participant_safe_reason)
    );
    line!(
        "  Requested: {}",
        describe_fact(&view.cleanup.requested_at, |value| format_unix_millis(
            *value
        ))
    );
    line!(
        "  Delete acknowledged: {}",
        describe_fact(&view.cleanup.delete_acknowledged_at, |value| {
            format_unix_millis(*value)
        })
    );
    line!(
        "  Exact absence confirmed: {}",
        describe_fact(&view.cleanup.absence_confirmed_at, |value| {
            format_unix_millis(*value)
        })
    );
    line!();
    line!(
        "Sources are scoped: provider observations are coarse metadata; participant reports are authenticated but untrusted claims."
    );
    line!(
        "No packet capture is used. The control plane owns network lifecycle and does not join the lobby tailnet."
    );
    output
}

fn describe_fact<T>(fact: &Fact<T>, format_value: impl Fn(&T) -> String) -> String {
    let value = match fact.value.as_ref() {
        Some(value) => {
            let rendered = format_value(value);
            if wire_value(&fact.freshness) == "stale" {
                format!("STALE — {rendered}")
            } else {
                rendered
            }
        }
        None if wire_value(&fact.freshness) == "not_applicable" => "not applicable".to_owned(),
        None => format!(
            "unknown — {}",
            fact.unknown_reason
                .as_ref()
                .map_or_else(|| "reason unavailable".to_owned(), wire_value)
        ),
    };
    let as_of = fact
        .as_of
        .map_or_else(|| "not recorded".to_owned(), format_unix_millis);
    let received_at = fact
        .received_at
        .map_or_else(|| "not recorded".to_owned(), format_unix_millis);
    format!(
        "{value} [source: {}; assurance: {}; as of: {as_of}; received: {received_at}; freshness: {}]",
        wire_value(&fact.source),
        wire_value(&fact.assurance),
        wire_value(&fact.freshness)
    )
}

fn wire_value(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(value)) => value,
        Ok(value) => value.to_string(),
        Err(_) => "unknown".to_owned(),
    }
}

fn format_unix_millis(value: UnixMillis) -> String {
    format!("{} ms since Unix epoch", value.as_millis())
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
        .filter(|device| device.tags.iter().any(|tag| tag == &lobby.tag))
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
    if lobby.mode == ProvisioningMode::TailnetPerLobby {
        bail!(
            "tailnet-per-lobby cleanup requires manual remediation because spurfire-ctl never persisted the child OAuth secret"
        );
    }
    let client = TailscaleClient::from_env().await?;
    let devices = client.list_devices(&lobby.tailnet).await?;
    let matching = devices
        .into_iter()
        .filter(|device| device.tags.iter().any(|tag| tag == &lobby.tag))
        .collect::<Vec<_>>();
    for device in &matching {
        client.delete_device(&device.id).await?;
    }
    let deleted_devices = matching.len();

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
    use std::io::Cursor;

    use spurfire_protocol::{
        ApplicationQuality, FactAssurance, FactSource, Freshness, InspectedLobbyLifecycle,
        NetworkAuthority, NetworkBacking, NetworkCleanup, NetworkCounts, NetworkLifecycle,
        NetworkTruthLabel, ParticipantCleanupReason, RouteAggregate, UnknownReason,
        LOBBY_NETWORK_SCHEMA_VERSION,
    };

    use super::*;

    const LOBBY_ID: &str = "123e4567-e89b-42d3-a456-426614174000";

    fn control<T>(value: T, now: UnixMillis) -> Fact<T> {
        Fact::known(
            value,
            FactSource::ControlStore,
            FactAssurance::Authoritative,
            Some(now),
            now,
            Freshness::Current,
        )
    }

    fn derived<T>(value: T, now: UnixMillis) -> Fact<T> {
        Fact::known(
            value,
            FactSource::Derived,
            FactAssurance::Derived,
            Some(now),
            now,
            Freshness::Fresh,
        )
    }

    fn unknown<T>() -> Fact<T> {
        Fact::unknown(FactSource::None, UnknownReason::NeverObserved, None)
    }

    fn simulated_view() -> LobbyNetworkView {
        let now = UnixMillis::new(10_000);
        LobbyNetworkView {
            schema_version: LOBBY_NETWORK_SCHEMA_VERSION,
            lobby_id: LobbyId::parse(LOBBY_ID).unwrap(),
            served_at: now,
            truth_label: NetworkTruthLabel::SimulatedNoTailnet,
            backing: NetworkBacking::simulated(1, now),
            lobby_lifecycle: control(InspectedLobbyLifecycle::Forming, now),
            counts: NetworkCounts {
                roster_count: control(0, now),
                provider_enrolled_device_count: Fact::not_applicable(),
                provider_online_device_count: Fact::not_applicable(),
                fresh_reporter_count: derived(0, now),
                fresh_directional_observation_count: derived(0, now),
            },
            routes: RouteAggregate {
                expected_direction_count: derived(0, now),
                reported_direction_count: derived(0, now),
                direct_count: derived(0, now),
                peer_relay_count: derived(0, now),
                derp_relay_count: derived(0, now),
                unavailable_count: derived(0, now),
                unknown_count: derived(0, now),
                reachable_known_count: derived(0, now),
                direct_ratio_milli: Fact::not_applicable(),
            },
            application_quality: ApplicationQuality {
                sample_count: derived(0, now),
                application_rtt_ms_median: Fact::not_applicable(),
                application_rtt_ms_p95: Fact::not_applicable(),
                application_rtt_ms_worst: Fact::not_applicable(),
                application_loss_ppm_median: Fact::not_applicable(),
            },
            authority: NetworkAuthority {
                control_election: unknown(),
                last_accepted_heartbeat: unknown(),
                peer_reported_match_authority: unknown(),
            },
            cleanup: NetworkCleanup {
                network_lifecycle: NetworkLifecycle::Simulated,
                requested_at: Fact::not_applicable(),
                delete_acknowledged_at: Fact::not_applicable(),
                absence_confirmed_at: Fact::not_applicable(),
                participant_safe_reason: ParticipantCleanupReason::SimulatedNoTailnet,
            },
        }
    }

    #[test]
    fn inspect_cli_accepts_only_a_capability_file_or_stdin_path() {
        let cli = Cli::try_parse_from([
            "spurfire-ctl",
            "lobby",
            "inspect",
            "--lobby",
            LOBBY_ID,
            "--server",
            "https://control.example",
            "--cap-file",
            "-",
            "--json",
        ])
        .unwrap();
        let Command::Lobby {
            command: LobbyCommand::Inspect(args),
        } = cli.command
        else {
            panic!("expected inspect command");
        };
        assert_eq!(args.lobby.to_string(), LOBBY_ID);
        assert_eq!(args.cap_file, PathBuf::from("-"));

        let rejected = Cli::try_parse_from([
            "spurfire-ctl",
            "lobby",
            "inspect",
            "--lobby",
            LOBBY_ID,
            "--server",
            "https://control.example",
            "--capability",
            "capability-must-never-be-an-argv-option",
        ]);
        assert!(rejected.is_err());
    }

    #[test]
    fn capability_reader_accepts_one_base64url_value_and_redacts_debug() {
        let token = "A".repeat(MIN_CAPABILITY_CHARS);
        let mut input = Cursor::new(format!("{token}\r\n"));
        let capability = read_capability_from(&mut input).unwrap();
        assert_eq!(capability.expose_secret(), token);
        assert_eq!(format!("{capability:?}"), "SecretCapability(<redacted>)");
        assert!(!format!("{capability:?}").contains(&token));

        let mut multiple = Cursor::new(format!("{token}\n{token}\n"));
        assert!(read_capability_from(&mut multiple).is_err());
        let mut whitespace = Cursor::new(format!(" {token}\n"));
        assert!(read_capability_from(&mut whitespace).is_err());
        let mut too_short = Cursor::new("short\n");
        assert!(read_capability_from(&mut too_short).is_err());

        let mistaken_argv_value = PathBuf::from(&token);
        let error = read_capability(&mistaken_argv_value).unwrap_err();
        assert!(!format!("{error:#}").contains(&token));
    }

    #[test]
    fn inspection_endpoint_requires_https_and_appends_an_encoded_lobby_path() {
        let lobby_id = LobbyId::parse(LOBBY_ID).unwrap();
        let endpoint = inspection_endpoint("https://control.example/base/", lobby_id).unwrap();
        assert_eq!(
            endpoint.as_str(),
            format!("https://control.example/base/v1/lobbies/{LOBBY_ID}/network")
        );
        assert!(inspection_endpoint("http://control.example", lobby_id).is_err());
        assert!(inspection_endpoint("https://user@control.example", lobby_id).is_err());
        assert!(inspection_endpoint("https://control.example?token=no", lobby_id).is_err());
    }

    #[test]
    fn inspection_request_marks_authorization_sensitive_and_never_uses_the_url() {
        let token = "S".repeat(MIN_CAPABILITY_CHARS);
        let mut input = Cursor::new(token.as_bytes());
        let capability = read_capability_from(&mut input).unwrap();
        let client = inspection_client().unwrap();
        let endpoint =
            inspection_endpoint("https://control.example", LobbyId::parse(LOBBY_ID).unwrap())
                .unwrap();
        let request = build_inspection_request(&client, endpoint, &capability).unwrap();

        assert!(!request.url().as_str().contains(&token));
        let authorization = request.headers().get(AUTHORIZATION).unwrap();
        assert!(authorization.is_sensitive());
        assert_eq!(
            authorization.to_str().unwrap(),
            format!("{CAPABILITY_SCHEME} {token}")
        );
        assert!(!format!("{request:?}").contains(&token));
    }

    #[test]
    fn human_inspection_output_is_honest_for_simulation_and_unknowns() {
        let view = simulated_view();
        view.validate().unwrap();
        let output = render_inspection(&view);
        assert!(output.contains("SIMULATED — NO TAILNET EXISTS"));
        assert!(output.contains("Tailnet DNS name / FQDN: not applicable"));
        assert!(output.contains("no — lifecycle owner only; not joined"));
        assert!(output.contains("Provider-online devices: not applicable"));
        assert!(output.contains("APPLICATION QUALITY (nonce/reply measurements only)"));
        assert!(output.contains("does not join the lobby tailnet"));
        assert!(!output.contains("provider_tailnet_id"));
        assert!(!output.contains("private_endpoint"));

        let json = serde_json::to_value(view).unwrap();
        assert_eq!(
            json["application_quality"]["application_rtt_ms_median"]["value"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn fact_rendering_retains_stale_values_and_explains_unknowns() {
        let now = UnixMillis::new(1_000);
        let stale = Fact::known(
            27_u32,
            FactSource::ProviderApi,
            FactAssurance::Observed,
            Some(now),
            now,
            Freshness::Stale,
        );
        let rendered = describe_fact(&stale, ToString::to_string);
        assert!(rendered.contains("STALE — 27"));
        assert!(rendered.contains("source: provider_api"));
        assert!(rendered.contains("freshness: stale"));

        let unknown =
            Fact::<u32>::unknown(FactSource::ProviderApi, UnknownReason::Timeout, Some(now));
        let rendered = describe_fact(&unknown, ToString::to_string);
        assert!(rendered.contains("unknown — timeout"));
        assert!(!rendered.starts_with('0'));
    }

    #[test]
    fn api_error_codes_are_bounded_before_cli_display() {
        assert_eq!(
            safe_error_code(br#"{"code":"lobby_not_found","message":"safe"}"#).as_deref(),
            Some("lobby_not_found")
        );
        assert_eq!(safe_error_code(br#"{"code":"TOKEN LEAK"}"#), None);
        assert_eq!(safe_error_code(br#"{"message":"missing"}"#), None);
    }

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
