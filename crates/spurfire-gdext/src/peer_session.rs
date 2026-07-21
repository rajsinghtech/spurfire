//! Godot-facing background RustScale application-UDP node.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    net::{IpAddr, SocketAddr},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use ed25519_dalek::{Signer, SigningKey};
use godot::classes::{INode, Node};
use godot::prelude::*;
use sha2::{Digest, Sha256};
use spurfire_net::{
    decode, encode,
    rustscale::RustScalePeer,
    v2::{
        decode_m3, encode_m3, fragment_m3_checkpoint, M3ActorInput, M3ActorLoadout,
        M3ActorSnapshot, M3HorseSnapshot, M3PeerPayloadV2, M3SecureSession,
    },
    AcceptOutcome, M3MatchCheckpointV2, MatchCheckpoint, PeerPayload, SecureSession, SessionState,
};
use spurfire_protocol::{
    canonical_keyreg_digest, ActorM3Mode, ActorM3TickInput, BountyMatchKernel, CombatAuthority,
    CombatGait, DiveId, EntityId, HorseVitalityClass, HorseVitalityState, LobbyId,
    LobbySessionProjection, M3ActorStance, M3AuthorityBank, M3AuthorityShot, M3CombatAuthority,
    M3HorseTargetPose, NodeKey, OnFootState, OnFootTickInput, PlayerId, QuantizedDirection,
    QuantizedOrigin, RecallState, RiderSnapshot as CombatRiderSnapshot, RiderStance, RidingState,
    RosterManifest, RosterManifestEntry, SessionPublicKey, SessionSignature, ShotCommand,
    ShotOutcome, ShotResult, SimulationTick, SpurCreditKind, SpurSpendOutcome, TargetDefinition,
    TargetPoseSnapshot, TargetRegistry, TeamId, WeaponAmmo, WeaponId, DIRECTION_UNITS,
    M3_WIRE_VERSION,
};
use tokio::{
    sync::mpsc::{self as tokio_mpsc, UnboundedReceiver, UnboundedSender},
    time::Duration,
};
use zeroize::Zeroizing;

use crate::{
    lobby_client::{
        route_for, safe_error, unix_millis, LobbyClientState, LobbyEvent, LobbyOperation,
        NativeLobbyError, NativeSecretInput,
    },
    saddle_dive_controller::SaddleDiveController,
};
use reqwest::Method;

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn rider_entity_id(player_id: PlayerId) -> EntityId {
    let digest = Sha256::digest(player_id.as_bytes());
    EntityId(u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    ))
}

fn horse_entity_id(player_id: PlayerId) -> EntityId {
    let mut digest = Sha256::new();
    digest.update(b"spurfire:m3:horse:");
    digest.update(player_id.as_bytes());
    let digest = digest.finalize();
    EntityId(u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    ))
}

#[derive(serde::Deserialize)]
struct M3LoadoutConfiguration {
    player_id: PlayerId,
    horse_class: HorseVitalityClass,
    weapon_id: WeaponId,
}

fn snapshot_dive_id(stance: RiderStance, raw_dive_id: i64) -> Option<Option<DiveId>> {
    match stance {
        RiderStance::SaddleDiveAirborne => u64::try_from(raw_dive_id)
            .ok()
            .and_then(DiveId::new)
            .map(Some),
        _ if raw_dive_id < 0 => Some(None),
        _ => None,
    }
}

fn rider_target_definition(player_id: PlayerId) -> TargetDefinition {
    TargetDefinition {
        entity_id: rider_entity_id(player_id),
        owner_peer_id: Some(player_id),
        // The Alpha is free-for-all. Shooter snapshots use team zero, while
        // hittable riders use team one; owner exclusion still prevents self hits.
        team_id: TeamId(1),
        max_health: 100,
    }
}

enum WorkerCommand {
    Send {
        packet: Vec<u8>,
        destination: SocketAddr,
    },
    QueryRoute {
        peer_ip: IpAddr,
    },
    Stop,
}

enum WorkerEvent {
    Connected {
        ip: String,
        port: u16,
    },
    Packet {
        packet: Vec<u8>,
        source: SocketAddr,
        node_key: Option<NodeKey>,
    },
    Route {
        peer_ip: String,
        route: String,
    },
    Failed(String),
    Stopped,
}

/// Tags every worker event with the session generation that spawned the
/// worker. The Godot thread drops events from an older generation so an
/// enrollment that lost a leave/quit race can never resurrect cleared state.
struct WorkerEventSink {
    generation: u64,
    sender: Sender<(u64, WorkerEvent)>,
}

impl WorkerEventSink {
    fn send(&self, event: WorkerEvent) {
        // Delivery is best-effort: the Godot thread may already be gone.
        let _ = self.sender.send((self.generation, event));
    }
}

/// Parse a destination endpoint for either address family. Building
/// `"{ip}:{port}"` is invalid for bare IPv6 literals, which previously made
/// every outbound packet to an IPv6-selected peer fail silently.
fn parse_destination(destination_ip: &str, port: i64) -> Option<SocketAddr> {
    let ip = destination_ip.parse::<IpAddr>().ok()?;
    let port = u16::try_from(port).ok()?;
    Some(SocketAddr::new(ip, port))
}

fn resolve_authority_shot(
    authority: &mut CombatAuthority,
    targets: &mut TargetRegistry,
    history: &BTreeMap<PlayerId, VecDeque<CombatRiderSnapshot>>,
    command: &ShotCommand,
) -> Option<ShotResult> {
    let rider = history
        .get(&command.shooter_peer_id)?
        .iter()
        .find(|snapshot| snapshot.tick == command.tick)
        .copied()?;
    Some(
        authority
            .validate_shot(command, command.tick, rider, targets)
            .result,
    )
}

fn resolve_m3_authority_shot(
    authority: &mut M3CombatAuthority,
    history: &BTreeMap<PlayerId, VecDeque<CombatRiderSnapshot>>,
    command: &ShotCommand,
    authority_tick: SimulationTick,
) -> Option<M3AuthorityShot> {
    let rider = history
        .get(&command.shooter_peer_id)?
        .iter()
        .find(|snapshot| snapshot.tick == command.tick)
        .copied()?;
    authority.resolve_shot(command, authority_tick, rider).ok()
}

fn quantized_planar_input(value: Vector2) -> Option<QuantizedDirection> {
    if !value.is_finite() || value.length_squared() <= f32::EPSILON {
        return None;
    }
    let normalized = value.normalized();
    Some(QuantizedDirection::new(
        (normalized.x * DIRECTION_UNITS as f32).round() as i32,
        0,
        (normalized.y * DIRECTION_UNITS as f32).round() as i32,
    ))
    .filter(|direction| direction.is_normalized())
}

async fn close_peer(peer: &mut RustScalePeer) -> Result<(), String> {
    let mut last = None;
    for _ in 0..4 {
        match peer.close().await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error.to_string());
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    let error = last.unwrap_or_else(|| "unknown RustScale shutdown failure".into());
    if error.contains("portmapper cleanup remains uncertain") {
        Ok(())
    } else {
        Err(error)
    }
}

/// Execute one queued gameplay command. Returns true when the worker must exit.
async fn handle_command(
    peer: &RustScalePeer,
    command: WorkerCommand,
    events: &WorkerEventSink,
) -> bool {
    match command {
        WorkerCommand::Send {
            packet,
            destination,
        } => {
            if let Err(error) = peer.send_datagram(&packet, destination).await {
                events.send(WorkerEvent::Failed(error.to_string()));
            }
            false
        }
        WorkerCommand::QueryRoute { peer_ip } => {
            let route = peer.route_to(peer_ip).unwrap_or_else(|| "Unknown".into());
            events.send(WorkerEvent::Route {
                peer_ip: peer_ip.to_string(),
                route,
            });
            false
        }
        WorkerCommand::Stop => true,
    }
}

enum EnrollOutcome<T> {
    Connected(T),
    Failed(String),
    Cancelled,
}

/// Await RustScale enrollment while staying responsive to shutdown. The old
/// worker blocked inside `RustScalePeer::connect` until the control server
/// answered, so a leave/quit during enrollment could not cancel it: the
/// one-use credential stayed captive and a late `connected` event could
/// resurrect a session the UI had already torn down. Selecting on the command
/// channel lets `Stop` drop the in-flight connect future, which releases the
/// credential and suppresses the stale completion. Gameplay commands queued
/// meanwhile are deferred and replayed in order once connected.
async fn enroll_with_cancellation<T, E, F>(
    connect: F,
    commands: &mut UnboundedReceiver<WorkerCommand>,
    deferred: &mut Vec<WorkerCommand>,
) -> EnrollOutcome<T>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    tokio::pin!(connect);
    loop {
        tokio::select! {
            result = &mut connect => {
                // Drain anything queued during the enrollment window so no
                // gameplay frame is reordered behind post-connect traffic.
                loop {
                    match commands.try_recv() {
                        Ok(WorkerCommand::Stop)
                        | Err(tokio_mpsc::error::TryRecvError::Disconnected) => {
                            return EnrollOutcome::Cancelled;
                        }
                        Ok(command) => deferred.push(command),
                        Err(tokio_mpsc::error::TryRecvError::Empty) => break,
                    }
                }
                return match result {
                    Ok(peer) => EnrollOutcome::Connected(peer),
                    Err(error) => EnrollOutcome::Failed(error.to_string()),
                };
            }
            command = commands.recv() => match command {
                None | Some(WorkerCommand::Stop) => return EnrollOutcome::Cancelled,
                Some(command) => deferred.push(command),
            },
        }
    }
}

fn run_worker(
    hostname: String,
    auth_key: Zeroizing<Vec<u8>>,
    port: u16,
    generation: u64,
    mut commands: UnboundedReceiver<WorkerCommand>,
    events: Sender<(u64, WorkerEvent)>,
) {
    let events = WorkerEventSink {
        generation,
        sender: events,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            events.send(WorkerEvent::Failed(format!(
                "network runtime failed: {error}"
            )));
            return;
        }
    };
    runtime.block_on(async move {
        let mut deferred = Vec::new();
        let mut peer = match enroll_with_cancellation(
            RustScalePeer::connect(hostname, auth_key, port),
            &mut commands,
            &mut deferred,
        )
        .await
        {
            EnrollOutcome::Connected(peer) => peer,
            EnrollOutcome::Failed(error) => {
                events.send(WorkerEvent::Failed(error));
                return;
            }
            EnrollOutcome::Cancelled => {
                events.send(WorkerEvent::Stopped);
                return;
            }
        };
        events.send(WorkerEvent::Connected {
            ip: peer.tailnet_ip().to_string(),
            port: peer.local_addr().port(),
        });
        let mut stopping = false;
        // Replay in order whatever was queued while enrollment was in flight.
        for command in deferred {
            if handle_command(&peer, command, &events).await {
                stopping = true;
                break;
            }
        }
        if !stopping {
            'network: loop {
                // Drain every queued gameplay frame before waiting for receive. The
                // old one-command-per-25ms loop could create avoidable input latency
                // whenever physics frames arrived in a burst.
                loop {
                    match commands.try_recv() {
                        Ok(command) => {
                            if handle_command(&peer, command, &events).await {
                                break 'network;
                            }
                        }
                        Err(tokio_mpsc::error::TryRecvError::Disconnected) => break 'network,
                        Err(tokio_mpsc::error::TryRecvError::Empty) => break,
                    }
                }
                match peer.recv_datagram(Duration::from_millis(5)).await {
                    Ok((packet, source)) => {
                        let node_key = peer.node_key_for(source.ip());
                        events.send(WorkerEvent::Packet {
                            packet,
                            source,
                            node_key,
                        });
                    }
                    Err(spurfire_net::rustscale::RustScaleTransportError::Timeout) => {}
                    Err(error) => {
                        events.send(WorkerEvent::Failed(error.to_string()));
                        break;
                    }
                }
            }
        }
        if let Err(error) = close_peer(&mut peer).await {
            events.send(WorkerEvent::Failed(error));
        }
        events.send(WorkerEvent::Stopped);
    });
}

/// A non-blocking Godot `Node` that owns an embedded ephemeral RustScale peer.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct PeerSession {
    #[base]
    base: Base<Node>,
    #[export]
    gameplay_rider_path: NodePath,
    #[var(no_set)]
    connection_state: GString,
    #[var(no_set)]
    tailnet_ip: GString,
    #[var(no_set)]
    local_port: i64,
    #[var(no_set)]
    local_player_id: GString,
    #[var(no_set)]
    authority_player_id: GString,
    #[var(no_set)]
    authority_epoch: i64,
    session: Option<SessionState>,
    secure_session: Option<SecureSession>,
    m3_wire_active: bool,
    m3_secure_session: Option<M3SecureSession>,
    m3_loadouts: BTreeMap<PlayerId, M3ActorLoadout>,
    session_key: Option<Zeroizing<[u8; 32]>>,
    session_key_generation: u64,
    pinned_manifest_key: Option<(u64, SessionPublicKey)>,
    insecure_demo_mode: bool,
    allowed_players: BTreeSet<PlayerId>,
    command_tx: Option<UnboundedSender<WorkerCommand>>,
    event_rx: Option<Receiver<(u64, WorkerEvent)>>,
    worker_handle: Option<thread::JoinHandle<()>>,
    worker_generation: u64,
    combat_authority: Option<CombatAuthority>,
    combat_targets: Option<TargetRegistry>,
    authority_rider_history: BTreeMap<PlayerId, VecDeque<CombatRiderSnapshot>>,
    combat_receipts: BTreeSet<(u64, PlayerId, u64)>,
    m3_combat_authority: Option<M3CombatAuthority>,
    m5_match_authority: Option<BountyMatchKernel>,
    lobby_client: LobbyClientState,
    creator_join_display: Option<String>,
    creator_join_lobby: Option<String>,
}

impl PeerSession {
    fn connect_native(
        &mut self,
        hostname: String,
        enrollment: Zeroizing<Vec<u8>>,
        port: u16,
    ) -> bool {
        if self.command_tx.is_some() {
            return false;
        }
        let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();
        self.worker_generation = self.worker_generation.wrapping_add(1);
        let generation = self.worker_generation;
        let Ok(worker_handle) = thread::Builder::new()
            .name("spurfire-rustscale".into())
            .spawn(move || {
                run_worker(hostname, enrollment, port, generation, command_rx, event_tx)
            })
        else {
            return false;
        };
        self.command_tx = Some(command_tx);
        self.worker_handle = Some(worker_handle);
        self.event_rx = Some(event_rx);
        self.connection_state = "connecting".into();
        true
    }

    fn secret_input(&self, path: &str) -> Option<Gd<NativeSecretInput>> {
        self.base()
            .try_get_node_as::<NativeSecretInput>(&NodePath::from(path))
    }

    fn clean_public_name(value: &GString) -> Option<String> {
        let cleaned = value
            .to_string()
            .trim()
            .chars()
            .take(64)
            .collect::<String>();
        (!cleaned.is_empty()).then_some(cleaned)
    }

    fn poll_lobby_events(&mut self) {
        loop {
            let event = match self.lobby_client.try_event() {
                Ok(event) => event,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let generation = match &event {
                LobbyEvent::Public { generation, .. }
                | LobbyEvent::Created { generation, .. }
                | LobbyEvent::Invitation { generation, .. }
                | LobbyEvent::Joined { generation, .. }
                | LobbyEvent::Failed { generation, .. } => *generation,
            };
            if generation != self.lobby_client.generation() {
                continue;
            }
            match event {
                LobbyEvent::Public {
                    operation, json, ..
                } => {
                    if operation == LobbyOperation::Readiness {
                        let value =
                            serde_json::from_str::<serde_json::Value>(&json).unwrap_or_default();
                        let create = value
                            .get("real_lobby_creation_authorized")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let join = value
                            .get("real_lobby_join_authorized")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        self.signals().readiness_changed().emit(create, join);
                        continue;
                    }
                    let json = GString::from(&json);
                    match operation {
                        LobbyOperation::Lobby => self.signals().lobby_updated().emit(&json),
                        LobbyOperation::Network => self.signals().network_updated().emit(&json),
                        LobbyOperation::Endpoint => {
                            self.signals().endpoint_registered().emit(&json)
                        }
                        LobbyOperation::Report => self.signals().report_completed().emit(&json),
                        LobbyOperation::Start => self.signals().start_completed().emit(&json),
                        LobbyOperation::Heartbeat => {
                            self.signals().heartbeat_completed().emit(&json)
                        }
                        LobbyOperation::Leave => self.signals().leave_completed().emit(&json),
                        LobbyOperation::End => self.signals().end_completed().emit(&json),
                        _ => {}
                    }
                }
                LobbyEvent::Created {
                    public_json,
                    creator,
                    ..
                } => {
                    self.lobby_client.install_creator(creator);
                    self.signals()
                        .create_completed()
                        .emit(&GString::from(&public_json));
                }
                LobbyEvent::Invitation {
                    creator_join,
                    lobby_id,
                    invitation,
                    ..
                } => {
                    if creator_join {
                        let display = self.creator_join_display.take();
                        let expected = self.creator_join_lobby.take();
                        if let Some(display) =
                            display.filter(|_| expected.as_deref() == Some(lobby_id.as_str()))
                        {
                            self.lobby_client
                                .join_creator(&lobby_id, &display, invitation);
                        }
                    } else if self
                        .lobby_client
                        .copy_invitation(&lobby_id, &invitation)
                        .is_ok()
                    {
                        self.signals()
                            .invitation_copied()
                            .emit(&GString::from(&lobby_id));
                    } else {
                        let operation = GString::from("invitation");
                        let message = GString::from(safe_error());
                        let code = GString::from("clipboard");
                        self.signals()
                            .request_failed()
                            .emit(&operation, &message, &code);
                    }
                }
                LobbyEvent::Joined { joined, .. } => {
                    let player = self.lobby_client.player_id().unwrap_or_default();
                    let hostname = format!(
                        "spurfire-rider-{}",
                        player.chars().take(8).collect::<String>()
                    );
                    if self.connect_native(hostname, joined.enrollment.into_zeroizing(), 41_643) {
                        self.lobby_client.install_participant(joined.participant);
                        self.signals()
                            .join_completed()
                            .emit(&GString::from(&joined.public_json));
                    } else {
                        drop(joined.participant);
                        let operation = GString::from("join");
                        let message = GString::from(safe_error());
                        let code = GString::from("worker");
                        self.signals()
                            .request_failed()
                            .emit(&operation, &message, &code);
                    }
                }
                LobbyEvent::Failed {
                    operation, error, ..
                } => {
                    let operation = GString::from(operation.code());
                    let message = GString::from(safe_error());
                    let code = GString::from(error.code());
                    self.signals()
                        .request_failed()
                        .emit(&operation, &message, &code);
                }
            }
        }
    }
}

#[godot_api]
impl PeerSession {
    #[signal]
    fn connected(tailnet_ip: GString, local_port: i64);
    #[signal]
    fn packet_received(
        packet: PackedByteArray,
        source_ip: GString,
        source_port: i64,
        source_node_key: GString,
    );
    #[signal]
    fn route_updated(peer_ip: GString, route: GString);
    #[signal]
    fn connection_failed(message: GString);
    #[signal]
    fn disconnected();
    #[signal]
    fn session_identity_bound(local_player_id: GString, authority_epoch: i64);
    #[signal]
    fn readiness_changed(create_authorized: bool, join_authorized: bool);
    #[signal]
    fn create_completed(public_json: GString);
    #[signal]
    fn invitation_copied(lobby_id: GString);
    #[signal]
    fn join_completed(public_json: GString);
    #[signal]
    fn lobby_updated(public_json: GString);
    #[signal]
    fn network_updated(public_json: GString);
    #[signal]
    fn endpoint_registered(public_json: GString);
    #[signal]
    fn report_completed(public_json: GString);
    #[signal]
    fn start_completed(public_json: GString);
    #[signal]
    fn heartbeat_completed(public_json: GString);
    #[signal]
    fn leave_completed(public_json: GString);
    #[signal]
    fn end_completed(public_json: GString);
    #[signal]
    fn request_failed(operation: GString, safe_message: GString, safe_code: GString);

    /// Enable legacy unsigned packets only for an explicit local demo/test.
    #[func]
    fn set_insecure_demo_mode(&mut self, enabled: bool) {
        self.insecure_demo_mode = enabled;
        if !enabled {
            self.secure_session = None;
            self.m3_secure_session = None;
        }
    }

    /// Generate a native-only Ed25519 key for one exact session generation.
    #[func]
    fn generate_session_key(&mut self, session_generation: i64) -> bool {
        let Ok(session_generation) = u64::try_from(session_generation) else {
            return false;
        };
        if self.session_key_generation == session_generation && self.session_key.is_some() {
            return true;
        }
        let mut seed = Zeroizing::new([0_u8; 32]);
        if getrandom::getrandom(&mut *seed).is_err() {
            return false;
        }
        self.session_key = Some(seed);
        self.session_key_generation = session_generation;
        self.secure_session = None;
        self.m3_secure_session = None;
        true
    }

    /// Return only the public half of the current ephemeral session key.
    #[func]
    fn session_public_key(&self) -> GString {
        let Some(seed) = &self.session_key else {
            return GString::new();
        };
        let public =
            SessionPublicKey::from_bytes(SigningKey::from_bytes(seed).verifying_key().to_bytes());
        let encoded = serde_json::to_string(&public).unwrap_or_default();
        GString::from(encoded.trim_matches('"'))
    }

    /// Produce a capability-bound registration proof without exporting the key.
    #[func]
    fn key_proof(
        &self,
        lobby_id: GString,
        player_id: GString,
        network_generation: i64,
        roster_revision: i64,
        address: GString,
        port: i64,
    ) -> GString {
        let (
            Some(seed),
            Ok(lobby_id),
            Ok(player_id),
            Ok(network_generation),
            Ok(roster_revision),
            Ok(address),
            Ok(port),
        ) = (
            &self.session_key,
            LobbyId::parse(&lobby_id.to_string()),
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(network_generation),
            u64::try_from(roster_revision),
            address.to_string().parse::<IpAddr>(),
            u16::try_from(port),
        )
        else {
            return GString::new();
        };
        let signing = SigningKey::from_bytes(seed);
        let public = SessionPublicKey::from_bytes(signing.verifying_key().to_bytes());
        let digest = canonical_keyreg_digest(
            lobby_id,
            player_id,
            network_generation,
            roster_revision,
            address,
            port,
            public,
        );
        let proof = SessionSignature::from_bytes(signing.sign(&digest).to_bytes());
        let encoded = serde_json::to_string(&proof).unwrap_or_default();
        GString::from(encoded.trim_matches('"'))
    }

    /// Pin or rotate the server manifest key. A changed key is accepted only
    /// with a strictly newer session generation or before any secure manifest.
    #[func]
    fn bind_manifest_key(&mut self, encoded_key: GString, session_generation: i64) -> bool {
        let Ok(session_generation) = u64::try_from(session_generation) else {
            return false;
        };
        let Ok(key) = serde_json::from_str::<SessionPublicKey>(&format!("\"{}\"", encoded_key))
        else {
            return false;
        };
        if let Some((generation, pinned)) = self.pinned_manifest_key {
            if key != pinned
                && session_generation <= generation
                && (self.secure_session.is_some() || self.m3_secure_session.is_some())
            {
                return false;
            }
        }
        self.pinned_manifest_key = Some((session_generation, key));
        true
    }

    /// Verify and install a complete server-signed projection.
    #[func]
    fn configure_secure_session(
        &mut self,
        lobby_id: GString,
        projection_json: GString,
        now_ms: i64,
    ) -> bool {
        let (
            Ok(lobby_id),
            Ok(projection),
            Ok(now_ms),
            Some(seed),
            Some((pinned_generation, pinned_key)),
        ) = (
            LobbyId::parse(&lobby_id.to_string()),
            serde_json::from_str::<LobbySessionProjection>(&projection_json.to_string()),
            u64::try_from(now_ms),
            &self.session_key,
            self.pinned_manifest_key,
        )
        else {
            return false;
        };
        if !projection.secure
            || projection.session_generation != self.session_key_generation
            || projection.session_generation != pinned_generation
            || projection.manifest_public_key != pinned_key
            || projection.roster_hash.is_none()
        {
            return false;
        }
        let Some(signature) = projection.manifest_signature else {
            return false;
        };
        let mut entries = Vec::with_capacity(projection.peers.len());
        for peer in projection.peers {
            let (Some(session_public_key), Ok(tailnet_address)) =
                (peer.session_public_key, peer.tailnet_address.parse())
            else {
                return false;
            };
            entries.push(RosterManifestEntry {
                player_id: peer.player_id,
                session_public_key,
                tailnet_address,
                application_port: peer.application_port,
                node_key: peer.node_key,
            });
        }
        let manifest = RosterManifest {
            lobby_id,
            network_generation: projection.network_generation,
            session_generation: projection.session_generation,
            roster_revision: projection.roster_revision,
            entries,
        };
        if manifest.hash() != projection.roster_hash.expect("checked present")
            || manifest.entries.len() != self.allowed_players.len()
            || manifest
                .entries
                .iter()
                .any(|entry| !self.allowed_players.contains(&entry.player_id))
        {
            return false;
        }
        let Some(local_player) = PlayerId::parse(&self.local_player_id.to_string()).ok() else {
            return false;
        };
        let Some(authority) = PlayerId::parse(&self.authority_player_id.to_string()).ok() else {
            return false;
        };
        let public = SigningKey::from_bytes(seed).verifying_key().to_bytes();
        if !manifest.entries.iter().any(|entry| {
            entry.player_id == local_player && entry.session_public_key.as_bytes() == &public
        }) {
            return false;
        }
        if self.m3_wire_active && self.m3_combat_authority.is_none() {
            return false;
        }
        let existing_state = if self.m3_wire_active {
            self.m3_secure_session.as_ref().and_then(|existing| {
                (existing.roster_hash() == manifest.hash()).then(|| existing.state().clone())
            })
        } else {
            self.secure_session.as_ref().and_then(|existing| {
                (existing.roster_hash() == manifest.hash()).then(|| existing.state().clone())
            })
        };
        let state = existing_state.unwrap_or_else(|| {
            let mut state = SessionState::new(lobby_id, local_player, authority, now_ms);
            for player in &self.allowed_players {
                state.add_peer(*player, now_ms);
            }
            state
        });
        if self.m3_wire_active {
            let Ok(secure) = M3SecureSession::new(manifest, pinned_key, signature, state) else {
                return false;
            };
            self.m3_secure_session = Some(secure);
            self.secure_session = None;
        } else {
            let Ok(secure) = SecureSession::new(manifest, pinned_key, signature, state) else {
                return false;
            };
            self.secure_session = Some(secure);
            self.m3_secure_session = None;
        }
        true
    }

    /// Configure validated application identity and deterministic authority state.
    #[func]
    fn configure_session(
        &mut self,
        lobby_id: GString,
        local_player_id: GString,
        authority_player_id: GString,
        now_ms: i64,
    ) -> bool {
        let roster = if local_player_id == authority_player_id {
            PackedStringArray::from([local_player_id.clone()])
        } else {
            PackedStringArray::from([local_player_id.clone(), authority_player_id.clone()])
        };
        self.configure_roster_session(
            lobby_id,
            local_player_id,
            authority_player_id,
            roster,
            now_ms,
        )
    }

    /// Bind packet acceptance to the exact control-plane roster. Unknown
    /// tailnet members are rejected before SessionState can observe them.
    #[func]
    fn configure_roster_session(
        &mut self,
        lobby_id: GString,
        local_player_id: GString,
        authority_player_id: GString,
        roster_player_ids: PackedStringArray,
        now_ms: i64,
    ) -> bool {
        let Ok(lobby_id) = LobbyId::parse(&lobby_id.to_string()) else {
            return false;
        };
        let Ok(local_player) = PlayerId::parse(&local_player_id.to_string()) else {
            return false;
        };
        let Ok(authority) = PlayerId::parse(&authority_player_id.to_string()) else {
            return false;
        };
        let Ok(now_ms) = u64::try_from(now_ms) else {
            return false;
        };
        let mut allowed_players = BTreeSet::new();
        for value in roster_player_ids.as_slice() {
            let Ok(player) = PlayerId::parse(&value.to_string()) else {
                return false;
            };
            if !allowed_players.insert(player) {
                return false;
            }
        }
        if allowed_players.is_empty()
            || !allowed_players.contains(&local_player)
            || !allowed_players.contains(&authority)
        {
            return false;
        }
        let mut combat_targets = match TargetRegistry::new(60) {
            Ok(targets) => targets,
            Err(_) => return false,
        };
        for player in &allowed_players {
            // A truncated cryptographic ID collision must fail closed rather
            // than alias two authority-owned health records.
            if combat_targets
                .register(rider_target_definition(*player))
                .is_err()
            {
                return false;
            }
        }
        let authority_epoch = 1_u64;
        if let Some(mut rider) = self
            .base()
            .try_get_node_as::<SaddleDiveController>(&self.gameplay_rider_path)
        {
            if !rider
                .bind_mut()
                .bind_session_identity(local_player, authority_epoch)
            {
                return false;
            }
        }
        let mut session = SessionState::new(lobby_id, local_player, authority, now_ms);
        for player in &allowed_players {
            session.add_peer(*player, now_ms);
        }
        self.session = Some(session);
        self.secure_session = None;
        self.m3_wire_active = false;
        self.m3_secure_session = None;
        self.m3_loadouts.clear();
        self.allowed_players = allowed_players;
        self.local_player_id = GString::from(&local_player.to_string());
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        self.combat_authority = CombatAuthority::new(60, 0).ok();
        if let Some(combat) = self.combat_authority.as_mut() {
            if !combat.set_authority_epoch(authority_epoch) {
                return false;
            }
            // Multiplayer loadouts are authority-owned. Until signed loadout
            // replication exists, every roster member starts with Dustwalker;
            // a fire command can never select or change this state.
            for player in &self.allowed_players {
                combat.register_shooter(*player, WeaponId::Dustwalker);
            }
        }
        self.combat_targets = Some(combat_targets);
        self.authority_rider_history.clear();
        self.combat_receipts.clear();
        self.m3_combat_authority = None;
        let signal_player = self.local_player_id.clone();
        let signal_epoch = self.authority_epoch;
        self.signals()
            .session_identity_bound()
            .emit(&signal_player, signal_epoch);
        true
    }

    /// Atomically replaces the retained wire-1.2 proof owner with the live M3
    /// actor/combat graph. This must happen before secure manifest binding.
    #[func]
    fn activate_m3_wire(&mut self, loadouts_json: GString) -> bool {
        if self.session.is_none()
            || self.m3_wire_active
            || self.secure_session.is_some()
            || self.m3_secure_session.is_some()
            || self.allowed_players.is_empty()
        {
            return false;
        }
        let Ok(rows) =
            serde_json::from_str::<Vec<M3LoadoutConfiguration>>(&loadouts_json.to_string())
        else {
            return false;
        };
        let mut loadouts = BTreeMap::new();
        for row in rows {
            if !self.allowed_players.contains(&row.player_id)
                || loadouts
                    .insert(row.player_id, (row.horse_class, row.weapon_id))
                    .is_some()
            {
                return false;
            }
        }
        if loadouts.len() != self.allowed_players.len() {
            return false;
        }
        let Ok(epoch) = u64::try_from(self.authority_epoch) else {
            return false;
        };
        let Ok(mut authority) = M3CombatAuthority::new(60, 0, epoch) else {
            return false;
        };
        let Ok(bounty) = BountyMatchKernel::new(
            epoch,
            0,
            SimulationTick::new(0),
            self.allowed_players.iter().copied().collect(),
        ) else {
            return false;
        };
        for player in &self.allowed_players {
            let Some((horse_class, weapon_id)) = loadouts.get(player).copied() else {
                return false;
            };
            if !authority.register_actor(
                *player,
                rider_entity_id(*player),
                horse_entity_id(*player),
                horse_class,
                weapon_id,
                TeamId(1),
            ) {
                return false;
            }
        }
        self.m3_combat_authority = Some(authority);
        self.m5_match_authority = Some(bounty);
        self.m3_loadouts = loadouts
            .into_iter()
            .map(|(player_id, (horse_class, weapon_id))| {
                (
                    player_id,
                    M3ActorLoadout {
                        horse_class,
                        weapon_id,
                    },
                )
            })
            .collect();
        self.combat_authority = None;
        self.combat_targets = None;
        self.combat_receipts.clear();
        self.m3_wire_active = true;
        true
    }

    #[func]
    fn is_m3_wire_active(&self) -> bool {
        self.m3_wire_active
    }

    /// Advance the authority-owned M5 match clock once after all actor ticks.
    #[func]
    fn advance_m5_match(&mut self, tick: i64) -> VarDictionary {
        let mut result = VarDictionary::new();
        if !self.m3_wire_active
            || self.local_player_id.to_string() != self.authority_player_id.to_string()
        {
            return result;
        }
        let Ok(tick) = u64::try_from(tick).map(SimulationTick::new) else {
            return result;
        };
        let (Some(bounty), Some(combat)) = (
            self.m5_match_authority.as_ref(),
            self.m3_combat_authority.as_ref(),
        ) else {
            return result;
        };
        let mut bounty = bounty.clone();
        let mut combat = combat.clone();
        let Ok(output) = bounty.advance_tick(tick) else {
            return result;
        };
        for player in &output.respawned_players {
            if combat.respawn_rider(*player).is_err() {
                return VarDictionary::new();
            }
        }
        self.m5_match_authority = Some(bounty.clone());
        self.m3_combat_authority = Some(combat);
        result.set("advanced", true);
        result.set(
            "remaining_ticks",
            i64::try_from(bounty.end_tick().as_u64().saturating_sub(tick.as_u64()))
                .unwrap_or(i64::MAX),
        );
        result.set("match_ended", output.match_ended);
        result.set(
            "winner_player_id",
            output
                .winner
                .map_or_else(String::new, |player| player.to_string()),
        );
        let mut respawned_players = PackedStringArray::new();
        for player in output.respawned_players {
            respawned_players.push(&GString::from(&player.to_string()));
        }
        result.set("respawned_players", &respawned_players);
        if let Some(reveal) = bounty.active_reveal() {
            result.set("most_wanted_player_id", reveal.player_id.to_string());
            result.set(
                "most_wanted_end_tick",
                i64::try_from(reveal.end_tick.as_u64()).unwrap_or(i64::MAX),
            );
        }
        if let Some(objective) = bounty.active_objective() {
            result.set(
                "objective_json",
                serde_json::to_string(&objective).unwrap_or_default(),
            );
        }
        result.set(
            "scoreboard_json",
            serde_json::to_string(&bounty.players().collect::<Vec<_>>()).unwrap_or_default(),
        );
        result
    }

    /// Build a bounded, sequenced heartbeat datagram for `send_packet`.
    #[func]
    fn make_heartbeat(&mut self, tick: i64) -> PackedByteArray {
        if self.m3_wire_active {
            self.make_m3_packet(tick, M3PeerPayloadV2::Heartbeat)
        } else {
            self.make_packet(tick, PeerPayload::Heartbeat)
        }
    }

    /// Build an application-channel RTT probe or response.
    #[func]
    fn make_probe(&mut self, tick: i64, nonce: i64, reply: bool) -> PackedByteArray {
        let Ok(nonce) = u64::try_from(nonce) else {
            return PackedByteArray::new();
        };
        if self.m3_wire_active {
            self.make_m3_packet(tick, M3PeerPayloadV2::Probe { nonce, reply })
        } else {
            self.make_packet(tick, PeerPayload::Probe { nonce, reply })
        }
    }

    /// Build bounded fixed-tick rider input; milli inputs must be in [-1000, 1000].
    #[func]
    fn make_rider_input(
        &mut self,
        tick: i64,
        throttle_milli: i64,
        steer_milli: i64,
        buttons: i64,
    ) -> PackedByteArray {
        if self.m3_wire_active {
            return PackedByteArray::new();
        }
        let (Ok(throttle_milli), Ok(steer_milli), Ok(buttons)) = (
            i16::try_from(throttle_milli),
            i16::try_from(steer_milli),
            u16::try_from(buttons),
        ) else {
            return PackedByteArray::new();
        };
        if !(-1_000..=1_000).contains(&throttle_milli)
            || !(-1_000..=1_000).contains(&steer_milli)
            || buttons & !0b11 != 0
        {
            return PackedByteArray::new();
        }
        self.make_packet(
            tick,
            PeerPayload::RiderInput {
                throttle_milli,
                steer_milli,
                buttons,
            },
        )
    }

    /// Build a quantized authority rider snapshot.
    #[func]
    fn make_rider_snapshot(
        &mut self,
        tick: i64,
        rider_player_id: GString,
        position: Vector3,
        velocity: Vector3,
        yaw_degrees: f64,
        stance_id: i64,
    ) -> PackedByteArray {
        if self.m3_wire_active {
            return PackedByteArray::new();
        }
        fn millimetres(value: f32) -> Option<i32> {
            let scaled = f64::from(value) * 1_000.0;
            scaled
                .is_finite()
                .then_some(scaled.round())
                .and_then(|value| {
                    (value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX))
                        .then_some(value as i32)
                })
        }
        let Ok(rider_player_id) = PlayerId::parse(&rider_player_id.to_string()) else {
            return PackedByteArray::new();
        };
        if !self.allowed_players.contains(&rider_player_id) {
            return PackedByteArray::new();
        }
        let Some(position_mm) = [position.x, position.y, position.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let Some(velocity_mmps) = [velocity.x, velocity.y, velocity.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let yaw = yaw_degrees * 1_000.0;
        let Ok(stance_id) = u8::try_from(stance_id) else {
            return PackedByteArray::new();
        };
        if !yaw.is_finite()
            || yaw < f64::from(i32::MIN)
            || yaw > f64::from(i32::MAX)
            || !(RiderStance::MOUNTED_ID..=RiderStance::ON_FOOT_STANDING_ID).contains(&stance_id)
        {
            return PackedByteArray::new();
        }
        self.make_packet(
            tick,
            PeerPayload::RiderSnapshot {
                rider_player_id,
                position_mm: [position_mm[0], position_mm[1], position_mm[2]],
                velocity_mmps: [velocity_mmps[0], velocity_mmps[1], velocity_mmps[2]],
                yaw_millidegrees: yaw.round() as i32,
                stance: RiderStance::from_u8(stance_id),
            },
        )
    }

    /// Build a shooter-bound command from the native combat controller JSON.
    #[func]
    fn make_shot_command(&mut self, tick: i64, command_json: GString) -> PackedByteArray {
        let Ok(command) = serde_json::from_str(&command_json.to_string()) else {
            return PackedByteArray::new();
        };
        if self.m3_wire_active {
            self.make_m3_packet(tick, M3PeerPayloadV2::ShotCommand { command })
        } else {
            self.make_packet(tick, PeerPayload::ShotCommand { command })
        }
    }

    /// Build complete wire-v2 mounted/on-foot intent. All milli axes and
    /// assigned button bits are validated by the native codec.
    #[func]
    #[allow(clippy::too_many_arguments)]
    fn make_m3_actor_input(
        &mut self,
        tick: i64,
        throttle_milli: i64,
        steer_milli: i64,
        move_x_milli: i64,
        move_z_milli: i64,
        buttons: i64,
    ) -> PackedByteArray {
        let (Ok(throttle_milli), Ok(steer_milli), Ok(move_x_milli), Ok(move_z_milli), Ok(buttons)) = (
            i16::try_from(throttle_milli),
            i16::try_from(steer_milli),
            i16::try_from(move_x_milli),
            i16::try_from(move_z_milli),
            u16::try_from(buttons),
        ) else {
            return PackedByteArray::new();
        };
        self.make_m3_packet(
            tick,
            M3PeerPayloadV2::ActorInput {
                input: M3ActorInput {
                    throttle_milli,
                    steer_milli,
                    move_x_milli,
                    move_z_milli,
                    buttons,
                },
            },
        )
    }

    /// Sign one authority-owned complete actor/horse snapshot.
    #[func]
    fn make_m3_actor_snapshot(&mut self, tick: i64, snapshot_json: GString) -> PackedByteArray {
        let Ok(snapshot) = serde_json::from_str::<M3ActorSnapshot>(&snapshot_json.to_string())
        else {
            return PackedByteArray::new();
        };
        let Some(authority) = self.m3_combat_authority.as_ref() else {
            return PackedByteArray::new();
        };
        let Some(actor) = authority.actor(snapshot.rider_player_id) else {
            return PackedByteArray::new();
        };
        if authority.actors().horse_entity_id(snapshot.rider_player_id)
            != Some(snapshot.horse.entity_id)
            || snapshot.horse.class.max_health() != actor.horse().max_health()
            || snapshot.horse.health != actor.horse().health()
            || snapshot.horse.state != actor.horse().state()
            || snapshot.stamina_ticks != actor.on_foot().stamina_ticks()
            || snapshot.recall_state != actor.recall().state()
            || snapshot.recall_ready_tick != actor.recall().ready_tick()
            || snapshot.spur_meter != actor.spur().meter()
            || snapshot.charge_started_tick != actor.spur().charge_started_tick()
            || snapshot.charge_end_tick != actor.spur().charge_end_tick()
            || authority
                .targets()
                .health(rider_entity_id(snapshot.rider_player_id))
                != Some(snapshot.rider_health)
        {
            return PackedByteArray::new();
        }
        self.make_m3_packet(tick, M3PeerPayloadV2::ActorSnapshot { snapshot })
    }

    /// Announce the sender's authority-locked M3 loadout.
    #[func]
    fn make_m3_actor_loadout(&mut self, tick: i64, loadout_json: GString) -> PackedByteArray {
        let Ok(loadout) = serde_json::from_str::<M3ActorLoadout>(&loadout_json.to_string()) else {
            return PackedByteArray::new();
        };
        let (Ok(local), Some(authority)) = (
            PlayerId::parse(&self.local_player_id.to_string()),
            self.m3_combat_authority.as_ref(),
        ) else {
            return PackedByteArray::new();
        };
        if authority
            .actor(local)
            .is_none_or(|actor| actor.horse().max_health() != loadout.horse_class.max_health())
            || authority
                .combat()
                .shooter_kernel(local)
                .is_none_or(|kernel| kernel.equipped_weapon() != loadout.weapon_id)
        {
            return PackedByteArray::new();
        }
        self.make_m3_packet(tick, M3PeerPayloadV2::ActorLoadout { loadout })
    }

    /// Export the authority-owned fields needed to compose a presentation
    /// snapshot. Transform fields remain collision-resolved scene inputs; no
    /// health, lifecycle, loadout, or recall state crosses into Godot as an
    /// authoritative mutation surface.
    #[func]
    fn m3_actor_state_json(&self, player_id: GString) -> GString {
        let Ok(player_id) = PlayerId::parse(&player_id.to_string()) else {
            return GString::new();
        };
        let (Some(authority), Some(loadout)) = (
            self.m3_combat_authority.as_ref(),
            self.m3_loadouts.get(&player_id),
        ) else {
            return GString::new();
        };
        let (Some(actor), Some(rider_health)) = (
            authority.actor(player_id),
            authority.targets().health(rider_entity_id(player_id)),
        ) else {
            return GString::new();
        };
        let ready_tick = actor.recall().ready_tick().map(SimulationTick::as_u64);
        let value = serde_json::json!({
            "player_id": player_id,
            "horse_entity_id": horse_entity_id(player_id).0.to_string(),
            "horse_class": loadout.horse_class,
            "horse_state": actor.horse().state(),
            "horse_health": actor.horse().health(),
            "rider_health": rider_health,
            "stamina_ticks": actor.on_foot().stamina_ticks(),
            "recall_state": actor.recall().state(),
            "recall_ready_tick": ready_tick,
            "spur_meter": actor.spur().meter(),
            "charge_started_tick": actor.spur().charge_started_tick(),
            "charge_end_tick": actor.spur().charge_end_tick(),
            "actor_mode": actor.mode(),
            "on_foot_state": actor.on_foot().state(),
            "bolt_away_delta_mm": actor.horse().bolt_away_delta_mm(),
        });
        GString::from(&serde_json::to_string(&value).unwrap_or_default())
    }

    /// Compose and sign a complete M3 snapshot from collision-resolved scene
    /// poses plus the private authority-owned gameplay state.
    #[func]
    #[allow(clippy::too_many_arguments)]
    fn make_m3_actor_snapshot_from_pose(
        &mut self,
        tick: i64,
        player_id: GString,
        rider_position: Vector3,
        rider_velocity: Vector3,
        rider_yaw_degrees: f64,
        legacy_stance_id: i64,
        horse_position: Vector3,
        horse_velocity: Vector3,
        horse_yaw_degrees: f64,
    ) -> PackedByteArray {
        let millimetres = |value: f32| {
            let scaled = f64::from(value) * 1_000.0;
            (scaled.is_finite() && scaled >= f64::from(i32::MIN) && scaled <= f64::from(i32::MAX))
                .then_some(scaled.round() as i32)
        };
        let millidegrees = |value: f64| {
            let scaled = value * 1_000.0;
            (scaled.is_finite() && scaled >= f64::from(i32::MIN) && scaled <= f64::from(i32::MAX))
                .then_some(scaled.round() as i32)
        };
        let (Ok(player_id), Ok(tick), Some(rider_yaw), Some(horse_yaw)) = (
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(tick),
            millidegrees(rider_yaw_degrees),
            millidegrees(horse_yaw_degrees),
        ) else {
            return PackedByteArray::new();
        };
        let Some(rider_position_mm) = [rider_position.x, rider_position.y, rider_position.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let Some(rider_velocity_mmps) = [rider_velocity.x, rider_velocity.y, rider_velocity.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let Some(horse_position_mm) = [horse_position.x, horse_position.y, horse_position.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let Some(horse_velocity_mmps) = [horse_velocity.x, horse_velocity.y, horse_velocity.z]
            .map(millimetres)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            return PackedByteArray::new();
        };
        let Some(authority) = self.m3_combat_authority.as_ref() else {
            return PackedByteArray::new();
        };
        let (Some(actor), Some(loadout), Some(rider_health)) = (
            authority.actor(player_id),
            self.m3_loadouts.get(&player_id),
            authority.targets().health(rider_entity_id(player_id)),
        ) else {
            return PackedByteArray::new();
        };
        let stance = match actor.mode() {
            ActorM3Mode::Mounted => match legacy_stance_id {
                1 => M3ActorStance::Mounted,
                2 => M3ActorStance::MountedAirborne,
                3 => M3ActorStance::SaddleDiveAirborne,
                4 => M3ActorStance::LandingProne,
                5 => M3ActorStance::LandingRecovery,
                _ => return PackedByteArray::new(),
            },
            ActorM3Mode::SpookStunned => M3ActorStance::SpookStunned,
            ActorM3Mode::OnFoot | ActorM3Mode::ReturningHorse => match actor.on_foot().state() {
                OnFootState::SpookStunned => M3ActorStance::SpookStunned,
                OnFootState::Standing => M3ActorStance::OnFootStanding,
                OnFootState::Sprinting => M3ActorStance::OnFootSprinting,
                OnFootState::Crouched => M3ActorStance::OnFootCrouched,
                OnFootState::Rolling => M3ActorStance::OnFootRolling,
            },
        };
        let [bolt_x, bolt_z] = actor.horse().bolt_away_delta_mm();
        let Ok(bolt_away_direction) =
            QuantizedDirection::from_components(f64::from(bolt_x), 0.0, f64::from(bolt_z))
        else {
            return PackedByteArray::new();
        };
        let snapshot = M3ActorSnapshot {
            rider_player_id: player_id,
            rider_position_mm: [
                rider_position_mm[0],
                rider_position_mm[1],
                rider_position_mm[2],
            ],
            rider_velocity_mmps: [
                rider_velocity_mmps[0],
                rider_velocity_mmps[1],
                rider_velocity_mmps[2],
            ],
            rider_yaw_millidegrees: rider_yaw,
            stance,
            rider_health,
            stamina_ticks: actor.on_foot().stamina_ticks(),
            horse: M3HorseSnapshot {
                entity_id: horse_entity_id(player_id),
                class: loadout.horse_class,
                state: actor.horse().state(),
                position_mm: [
                    horse_position_mm[0],
                    horse_position_mm[1],
                    horse_position_mm[2],
                ],
                velocity_mmps: [
                    horse_velocity_mmps[0],
                    horse_velocity_mmps[1],
                    horse_velocity_mmps[2],
                ],
                yaw_millidegrees: horse_yaw,
                health: actor.horse().health(),
                bolt_away_direction,
            },
            recall_state: actor.recall().state(),
            recall_ready_tick: actor.recall().ready_tick(),
            spur_meter: actor.spur().meter(),
            charge_started_tick: actor.spur().charge_started_tick(),
            charge_end_tick: actor.spur().charge_end_tick(),
        };
        self.make_m3_packet(tick as i64, M3PeerPayloadV2::ActorSnapshot { snapshot })
    }

    /// Advance the private composed M3 actor owner before recording collision
    /// poses or resolving combat for the same authority tick.
    #[func]
    #[allow(clippy::too_many_arguments)]
    fn advance_m3_actor(
        &mut self,
        player_id: GString,
        tick: i64,
        move_input: Vector2,
        sprint_pressed: bool,
        crouch_pressed: bool,
        reload_active: bool,
        interact_pressed: bool,
        spur_pressed: bool,
        mounted_for_spur: bool,
        rider_position: Vector3,
        return_horse_position: Vector3,
        return_horse_moving: bool,
    ) -> VarDictionary {
        if !self.m3_wire_active
            || self.local_player_id.to_string() != self.authority_player_id.to_string()
        {
            return VarDictionary::new();
        }
        let (Ok(player_id), Ok(tick), Ok(rider_position), Ok(return_horse_position)) = (
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(tick).map(SimulationTick::new),
            QuantizedOrigin::from_meters(
                f64::from(rider_position.x),
                f64::from(rider_position.y),
                f64::from(rider_position.z),
            ),
            QuantizedOrigin::from_meters(
                f64::from(return_horse_position.x),
                f64::from(return_horse_position.y),
                f64::from(return_horse_position.z),
            ),
        ) else {
            return VarDictionary::new();
        };
        let input = ActorM3TickInput {
            tick,
            on_foot: OnFootTickInput {
                tick,
                move_direction: quantized_planar_input(move_input),
                sprint_pressed,
                crouch_pressed,
                reload_active,
            },
            interact_pressed,
            spur_pressed,
            mounted_for_spur,
            rider_position,
            return_horse_position,
            return_horse_moving,
        };
        let Some(authority) = self.m3_combat_authority.as_mut() else {
            return VarDictionary::new();
        };
        let Ok(output) = authority.advance_actor(player_id, input) else {
            return VarDictionary::new();
        };
        let Some(actor) = authority.actor(player_id) else {
            return VarDictionary::new();
        };
        let mut result = VarDictionary::new();
        result.set("advanced", true);
        result.set("tick", i64::try_from(tick.as_u64()).unwrap_or(i64::MAX));
        result.set("horse_health", i64::from(actor.horse().health()));
        result.set("horse_max_health", i64::from(actor.horse().max_health()));
        result.set(
            "horse_state_id",
            match actor.horse().state() {
                HorseVitalityState::Available => 0_i64,
                HorseVitalityState::Bolting => 1,
                HorseVitalityState::Despawned => 2,
            },
        );
        result.set(
            "horse_bolt_started_tick",
            actor.horse().bolt_started_tick().map_or(-1, |value| {
                i64::try_from(value.as_u64()).unwrap_or(i64::MAX)
            }),
        );
        let [bolt_x, bolt_z] = actor.horse().bolt_away_delta_mm();
        result.set(
            "horse_bolt_direction",
            Vector2::new(bolt_x as f32, bolt_z as f32).normalized(),
        );
        result.set("stamina_ticks", i64::from(actor.on_foot().stamina_ticks()));
        result.set("spur_meter", i64::from(actor.spur().meter()));
        result.set("charge_active", output.spur.charge_active);
        result.set("charge_ended", output.spur.charge_ended);
        result.set(
            "spur_spend_id",
            match output.spur.spend {
                None => 0_i64,
                Some(SpurSpendOutcome::MajesticCharge) => 1,
                Some(SpurSpendOutcome::InstantMajesticReturn) => 2,
            },
        );
        result.set(
            "charge_started_tick",
            actor.spur().charge_started_tick().map_or(-1, |value| {
                i64::try_from(value.as_u64()).unwrap_or(i64::MAX)
            }),
        );
        result.set(
            "charge_end_tick",
            actor.spur().charge_end_tick().map_or(-1, |value| {
                i64::try_from(value.as_u64()).unwrap_or(i64::MAX)
            }),
        );
        result.set(
            "recall_state_id",
            match actor.recall().state() {
                RecallState::HorsePresent => 0_i64,
                RecallState::CoolingDown => 1,
                RecallState::Ready => 2,
                RecallState::Hoofbeats => 3,
                RecallState::DustReveal => 4,
                RecallState::GallopIn => 5,
                RecallState::MountWindow => 6,
                RecallState::WaitingMount => 7,
            },
        );
        result.set(
            "recall_phase_enter_tick",
            actor.recall().phase_enter_tick().map_or(-1, |value| {
                i64::try_from(value.as_u64()).unwrap_or(i64::MAX)
            }),
        );
        result.set("horse_despawned", output.horse_despawned);
        result.set("remounted", output.remounted);
        result.set("running_mount", output.running_mount);
        if let Some(on_foot) = output.on_foot {
            result.set("on_foot_active", true);
            result.set(
                "on_foot_state_id",
                match on_foot.state {
                    OnFootState::SpookStunned => 0_i64,
                    OnFootState::Standing => 1,
                    OnFootState::Sprinting => 2,
                    OnFootState::Crouched => 3,
                    OnFootState::Rolling => 4,
                },
            );
            result.set(
                "on_foot_velocity",
                Vector2::new(
                    on_foot.requested_velocity_mmps[0] as f32 / 1_000.0,
                    on_foot.requested_velocity_mmps[1] as f32 / 1_000.0,
                ),
            );
            result.set("on_foot_can_fire", on_foot.can_fire);
            result.set("reload_pause_started", on_foot.reload_pause_started);
            result.set(
                "sway_multiplier_milli",
                i64::from(on_foot.sway_multiplier_milli + on_foot.roll_exit_sway_milli),
            );
        } else {
            result.set("on_foot_active", false);
        }
        if let Some(reload) = authority.reload(player_id) {
            result.set("reloading", true);
            result.set(
                "reload_active_ticks",
                i64::try_from(reload.active_ticks).unwrap_or(i64::MAX),
            );
            result.set(
                "reload_required_ticks",
                i64::try_from(reload.required_ticks).unwrap_or(i64::MAX),
            );
        } else {
            result.set("reloading", false);
            result.set("reload_active_ticks", 0);
            result.set("reload_required_ticks", 0);
        }
        result
    }

    /// Issue one authority-observed non-combat M4 award. Evidence is validated
    /// by the native Spur kernel; clients cannot submit meter deltas.
    #[func]
    fn issue_m4_spur_credit(
        &mut self,
        player_id: GString,
        tick: i64,
        source_id: i64,
        evidence_a: i64,
        evidence_b: i64,
    ) -> i64 {
        if !self.m3_wire_active
            || self.local_player_id.to_string() != self.authority_player_id.to_string()
        {
            return -1;
        }
        let (Ok(player_id), Ok(tick)) = (
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(tick).map(SimulationTick::new),
        ) else {
            return -1;
        };
        let kind = match source_id {
            0 => SpurCreditKind::Jump,
            1 => {
                let Ok(sway_impulse_milli) = u16::try_from(evidence_b) else {
                    return -1;
                };
                SpurCreditKind::CleanLanding {
                    collision_free: evidence_a == 1,
                    sway_impulse_milli,
                }
            }
            2 => {
                let Ok(closest_distance_mm) = u16::try_from(evidence_b) else {
                    return -1;
                };
                SpurCreditKind::NearMiss {
                    hostile: evidence_a == 1,
                    closest_distance_mm,
                }
            }
            _ => return -1,
        };
        self.m3_combat_authority
            .as_mut()
            .and_then(|authority| authority.issue_spur_credit(player_id, tick, kind).ok())
            .flatten()
            .map_or(-1, i64::from)
    }

    /// Record authority collision-resolved horse geometry for M3 rewind.
    #[func]
    fn record_m3_horse_pose(
        &mut self,
        player_id: GString,
        tick: i64,
        body_center: Vector3,
        body_forward: Vector3,
        head_center: Vector3,
    ) -> bool {
        let (Ok(player_id), Ok(tick), Ok(body_center), Ok(body_forward), Ok(head_center)) = (
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(tick).map(SimulationTick::new),
            QuantizedOrigin::from_meters(
                f64::from(body_center.x),
                f64::from(body_center.y),
                f64::from(body_center.z),
            ),
            QuantizedDirection::from_components(
                f64::from(body_forward.x),
                f64::from(body_forward.y),
                f64::from(body_forward.z),
            ),
            QuantizedOrigin::from_meters(
                f64::from(head_center.x),
                f64::from(head_center.y),
                f64::from(head_center.z),
            ),
        ) else {
            return false;
        };
        self.m3_wire_active
            && self.local_player_id.to_string() == self.authority_player_id.to_string()
            && self.m3_combat_authority.as_mut().is_some_and(|authority| {
                authority
                    .record_horse_pose(M3HorseTargetPose {
                        tick,
                        rider_player_id: player_id,
                        body_center,
                        body_forward,
                        head_center,
                    })
                    .is_ok()
            })
    }

    /// Record authority-simulated muzzle, target geometry, and rider state for
    /// one exact tick. Client shot commands never contribute to this history.
    #[func]
    fn record_authority_rider_snapshot(
        &mut self,
        player_id: GString,
        tick: i64,
        muzzle_origin: Vector3,
        rider_position: Vector3,
        velocity: Vector3,
        riding_state: PackedInt64Array,
    ) -> bool {
        if self.local_player_id.to_string() != self.authority_player_id.to_string() {
            return false;
        }
        let [stance_id, raw_dive_id] = riding_state.as_slice() else {
            return false;
        };
        let (
            Ok(player_id),
            Ok(tick),
            Ok(stance_id),
            Ok(muzzle_origin),
            Ok(body_center),
            Ok(head_center),
        ) = (
            PlayerId::parse(&player_id.to_string()),
            u64::try_from(tick).map(SimulationTick::new),
            u8::try_from(*stance_id),
            QuantizedOrigin::from_meters(
                f64::from(muzzle_origin.x),
                f64::from(muzzle_origin.y),
                f64::from(muzzle_origin.z),
            ),
            QuantizedOrigin::from_meters(
                f64::from(rider_position.x),
                f64::from(rider_position.y) + 0.8,
                f64::from(rider_position.z),
            ),
            QuantizedOrigin::from_meters(
                f64::from(rider_position.x),
                f64::from(rider_position.y) + 1.65,
                f64::from(rider_position.z),
            ),
        )
        else {
            return false;
        };
        if !self.allowed_players.contains(&player_id) {
            return false;
        }
        let stance = RiderStance::from_u8(stance_id);
        let Some(dive_id) = snapshot_dive_id(stance, *raw_dive_id) else {
            return false;
        };
        let planar_speed = Vector2::new(velocity.x, velocity.z).length();
        if !planar_speed.is_finite() {
            return false;
        }
        let planar_speed_mmps = (f64::from(planar_speed) * 1_000.0)
            .round()
            .clamp(0.0, f64::from(u32::MAX)) as u32;
        let gait = if planar_speed_mmps < 500 {
            CombatGait::Idle
        } else if planar_speed_mmps < 3_000 {
            CombatGait::Walk
        } else if planar_speed_mmps < 8_000 {
            CombatGait::Trot
        } else {
            CombatGait::Gallop
        };
        let majestic_charge = self
            .m3_combat_authority
            .as_ref()
            .and_then(|authority| authority.actor(player_id))
            .is_some_and(|actor| actor.spur().charge_active());
        let snapshot = CombatRiderSnapshot {
            tick,
            shooter_peer_id: player_id,
            muzzle_origin,
            team_id: TeamId::default(),
            riding: RidingState {
                stance,
                dive_id,
                gait,
                planar_speed_mmps,
                gait_top_speed_mmps: 13_000,
                yaw_rate_millidegrees_per_second: 0,
                stumbling: false,
                ads: false,
                sprint_gallop: false,
                majestic_charge,
            },
        };
        if !snapshot.riding.is_consistent() {
            return false;
        }
        let history = self.authority_rider_history.entry(player_id).or_default();
        if history.back().is_some_and(|previous| previous.tick >= tick) {
            return false;
        }
        let pose = TargetPoseSnapshot {
            tick,
            entity_id: rider_entity_id(player_id),
            stance,
            body_center,
            body_half_height_mm: 500,
            body_radius_mm: 350,
            head_center,
            head_radius_mm: 250,
            active: true,
        };
        let recorded = if self.m3_wire_active {
            self.m3_combat_authority
                .as_mut()
                .is_some_and(|authority| authority.record_rider_pose(player_id, pose).is_ok())
        } else {
            self.combat_targets
                .as_mut()
                .is_some_and(|targets| targets.record_pose(pose).is_ok())
        };
        if !recorded {
            return false;
        }
        history.push_back(snapshot);
        while history.len() > 32 {
            history.pop_front();
        }
        true
    }

    /// Resolve one admitted command exactly once through `CombatAuthority`.
    /// The returned JSON can only be signed by `make_shot_result` on authority.
    #[func]
    fn resolve_shot_command(&mut self, command_json: GString) -> GString {
        let Ok(command) = serde_json::from_str::<ShotCommand>(&command_json.to_string()) else {
            return GString::new();
        };
        if self.local_player_id.to_string() != self.authority_player_id.to_string()
            || !self.allowed_players.contains(&command.shooter_peer_id)
        {
            return GString::new();
        }
        let result = if self.m3_wire_active {
            let Some(authority) = self.m3_combat_authority.as_mut() else {
                return GString::new();
            };
            let Some(resolved) = resolve_m3_authority_shot(
                authority,
                &self.authority_rider_history,
                &command,
                command.tick,
            ) else {
                return GString::new();
            };
            resolved.shot.result
        } else {
            let (Some(authority), Some(targets)) =
                (self.combat_authority.as_mut(), self.combat_targets.as_mut())
            else {
                return GString::new();
            };
            let Some(result) =
                resolve_authority_shot(authority, targets, &self.authority_rider_history, &command)
            else {
                return GString::new();
            };
            result
        };
        let encoded = serde_json::to_string(&result).unwrap_or_default();
        GString::from(&encoded)
    }

    /// Resolve M3 combat at the current authority tick and expose both signed
    /// shot truth and routed horse effects to the native scene adapter.
    #[func]
    fn resolve_m3_shot_command(
        &mut self,
        command_json: GString,
        authority_tick: i64,
    ) -> VarDictionary {
        let (Ok(command), Ok(authority_tick)) = (
            serde_json::from_str::<ShotCommand>(&command_json.to_string()),
            u64::try_from(authority_tick).map(SimulationTick::new),
        ) else {
            return VarDictionary::new();
        };
        if !self.m3_wire_active
            || self.local_player_id.to_string() != self.authority_player_id.to_string()
            || !self.allowed_players.contains(&command.shooter_peer_id)
        {
            return VarDictionary::new();
        }
        let (Some(authority), Some(bounty)) = (
            self.m3_combat_authority.as_ref(),
            self.m5_match_authority.as_ref(),
        ) else {
            return VarDictionary::new();
        };
        if bounty.current_tick() != authority_tick || bounty.finished() {
            return VarDictionary::new();
        }
        let mut authority = authority.clone();
        let mut bounty = bounty.clone();
        let shooter_snapshot = self
            .authority_rider_history
            .get(&command.shooter_peer_id)
            .and_then(|history| history.iter().find(|row| row.tick == command.tick))
            .copied();
        let shooter_stance = shooter_snapshot.map(|row| row.riding.stance);
        let spur_before = authority
            .actor(command.shooter_peer_id)
            .map_or(0, |actor| actor.spur().meter());
        let Some(resolved) = resolve_m3_authority_shot(
            &mut authority,
            &self.authority_rider_history,
            &command,
            authority_tick,
        ) else {
            return VarDictionary::new();
        };
        let shooter_on_foot = authority
            .actor(command.shooter_peer_id)
            .is_some_and(|actor| actor.mode() != ActorM3Mode::Mounted);
        let spur_after = authority
            .actor(command.shooter_peer_id)
            .map_or(spur_before, |actor| actor.spur().meter());
        let shooter_charge_active = authority
            .actor(command.shooter_peer_id)
            .is_some_and(|actor| actor.spur().charge_active());
        let eliminated_rider = resolved
            .shot
            .result
            .target_id
            .filter(|_| resolved.shot.result.eliminated)
            .and_then(|target| authority.rider_owner(target));
        let target_on_foot = eliminated_rider.is_some_and(|player| {
            authority
                .actor(player)
                .is_some_and(|actor| actor.mode() != ActorM3Mode::Mounted)
        });
        let hit_rider = resolved
            .shot
            .result
            .target_id
            .and_then(|target| authority.rider_owner(target));
        let mut bounty_awards = Vec::new();
        if resolved.shot.result.outcome == ShotOutcome::Hit {
            if let Some(target) = hit_rider {
                if bounty
                    .record_damage(
                        authority_tick,
                        command.shooter_peer_id,
                        target,
                        resolved.shot.result.damage,
                    )
                    .is_err()
                {
                    return VarDictionary::new();
                }
            }
            if shooter_stance == Some(RiderStance::Mounted) {
                if let Some(distance) = resolved.shot.result.distance_mm {
                    match bounty.record_mounted_long_hit(
                        authority_tick,
                        command.shooter_peer_id,
                        distance,
                    ) {
                        Ok(Some(award)) => bounty_awards.push(award),
                        Ok(None) => {}
                        Err(_) => return VarDictionary::new(),
                    }
                }
            }
        }
        if let Some(horse_damage) = resolved.horse_damage {
            if horse_damage.effects.application.spooked {
                let Ok(award) = bounty.record_horse_bolt(
                    authority_tick,
                    command.shooter_peer_id,
                    horse_damage.rider_player_id,
                ) else {
                    return VarDictionary::new();
                };
                bounty_awards.push(award);
            }
        }
        if let Some(target) = eliminated_rider {
            let Ok(mut awards) = bounty.record_elimination(
                authority_tick,
                command.shooter_peer_id,
                target,
                shooter_stance == Some(RiderStance::SaddleDiveAirborne),
            ) else {
                return VarDictionary::new();
            };
            bounty_awards.append(&mut awards);
        }
        self.m3_combat_authority = Some(authority);
        self.m5_match_authority = Some(bounty.clone());
        let mut result = VarDictionary::new();
        result.set(
            "result_json",
            serde_json::to_string(&resolved.shot.result).unwrap_or_default(),
        );
        result.set("shooter_on_foot", shooter_on_foot);
        result.set(
            "spur_awarded_points",
            i64::from(spur_after.saturating_sub(spur_before)),
        );
        result.set("spur_meter", i64::from(spur_after));
        result.set("shooter_charge_active", shooter_charge_active);
        result.set(
            "bounty_awards_json",
            serde_json::to_string(&bounty_awards).unwrap_or_default(),
        );
        result.set(
            "scoreboard_json",
            serde_json::to_string(&bounty.players().collect::<Vec<_>>()).unwrap_or_default(),
        );
        if let Some(snapshot) = shooter_snapshot {
            let origin = snapshot.muzzle_origin.to_meters();
            result.set(
                "authority_origin",
                Vector3::new(origin[0] as f32, origin[1] as f32, origin[2] as f32),
            );
        }
        if spur_after > spur_before {
            let source = match (shooter_stance, resolved.shot.result.eliminated) {
                (Some(RiderStance::SaddleDiveAirborne), true) => "saddle_dive_elimination",
                (Some(RiderStance::Mounted), true) => "mounted_elimination",
                _ => "mounted_hit",
            };
            result.set("spur_award_source", source);
        }
        if let Some(player) = eliminated_rider {
            result.set("eliminated_rider_player_id", player.to_string());
            result.set("eliminated_rider_on_foot", target_on_foot);
        }
        if let Some(horse_damage) = resolved.horse_damage {
            result.set(
                "horse_damage_json",
                serde_json::to_string(&horse_damage).unwrap_or_default(),
            );
            result.set(
                "horse_rider_player_id",
                horse_damage.rider_player_id.to_string(),
            );
            result.set(
                "horse_entity_id",
                horse_damage.horse_entity_id.0.to_string(),
            );
        }
        result
    }

    /// Export authority-owned combat state for one checkpoint rider.
    #[func]
    fn combat_checkpoint_state(&self, player_id: GString) -> VarDictionary {
        let mut state = VarDictionary::new();
        let Ok(player_id) = PlayerId::parse(&player_id.to_string()) else {
            return state;
        };
        let authority = if self.m3_wire_active {
            let Some(authority) = self.m3_combat_authority.as_ref() else {
                return state;
            };
            authority.combat()
        } else {
            let Some(authority) = self.combat_authority.as_ref() else {
                return state;
            };
            authority
        };
        let Some(kernel) = authority.shooter_kernel(player_id) else {
            return state;
        };
        let ammo = kernel.equipped_ammo();
        state.set("weapon_id", i64::from(kernel.equipped_weapon().as_u8()));
        state.set("ammo_magazine", i64::from(ammo.magazine));
        state.set("ammo_reserve", i64::from(ammo.reserve));
        let health = if self.m3_wire_active {
            self.m3_combat_authority
                .as_ref()
                .and_then(|authority| authority.targets().health(rider_entity_id(player_id)))
        } else {
            self.combat_targets
                .as_ref()
                .and_then(|targets| targets.health(rider_entity_id(player_id)))
        };
        let Some(health) = health else {
            return VarDictionary::new();
        };
        state.set("health", i64::from(health));
        state.set(
            "last_shot_tick",
            kernel
                .last_accepted_tick()
                .map_or(-1, |tick| i64::try_from(tick.as_u64()).unwrap_or(i64::MAX)),
        );
        state.set(
            "last_command_tick",
            authority
                .last_command_tick(player_id)
                .map_or(-1, |tick| i64::try_from(tick.as_u64()).unwrap_or(i64::MAX)),
        );
        state.set(
            "shot_index",
            i64::try_from(kernel.shot_index()).unwrap_or(i64::MAX),
        );
        state
    }

    /// Export exact current-epoch result receipts for migration deduplication.
    #[func]
    fn combat_resolved_shots_json(&self) -> GString {
        let Ok(epoch) = u64::try_from(self.authority_epoch) else {
            return GString::new();
        };
        let rows = self
            .combat_receipts
            .iter()
            .filter_map(|(receipt_epoch, shooter, tick)| {
                (*receipt_epoch == epoch).then_some((*shooter, *tick))
            })
            .collect::<Vec<_>>();
        GString::from(&serde_json::to_string(&rows).unwrap_or_default())
    }

    /// Build authority-only resolved combat truth.
    #[func]
    fn make_shot_result(&mut self, tick: i64, result_json: GString) -> PackedByteArray {
        let Ok(result) = serde_json::from_str::<ShotResult>(&result_json.to_string()) else {
            return PackedByteArray::new();
        };
        let receipt = (
            u64::try_from(self.authority_epoch).ok(),
            result.shooter_peer_id,
            result.tick.as_u64(),
        );
        let packet = if self.m3_wire_active {
            self.make_m3_packet(tick, M3PeerPayloadV2::ShotResult { result })
        } else {
            self.make_packet(tick, PeerPayload::ShotResult { result })
        };
        if !packet.is_empty() {
            if let Some(epoch) = receipt.0 {
                self.combat_receipts.insert((epoch, receipt.1, receipt.2));
            }
        }
        packet
    }

    /// Advance exactly one epoch after timeout and sign a bounded checkpoint.
    #[func]
    fn poll_migration(&mut self, now_ms: i64, checkpoint_json: GString) -> PackedByteArray {
        if self.m3_wire_active {
            return PackedByteArray::new();
        }
        let (Ok(now_ms), Ok(checkpoint)) = (
            u64::try_from(now_ms),
            serde_json::from_str::<MatchCheckpoint>(&checkpoint_json.to_string()),
        ) else {
            return PackedByteArray::new();
        };
        if !checkpoint.is_bounded_and_canonical() {
            return PackedByteArray::new();
        }
        // Prepare the replacement before election state can mutate. A malformed
        // combat checkpoint therefore fails the migration atomically.
        let Some(next_epoch) = checkpoint.source_epoch.checked_add(1) else {
            return PackedByteArray::new();
        };
        let Some((restored_combat, restored_targets)) =
            Self::restore_combat_checkpoint(&checkpoint, next_epoch)
        else {
            return PackedByteArray::new();
        };
        let Some(secure) = self.secure_session.as_mut() else {
            return PackedByteArray::new();
        };
        let source_epoch = secure.state().authority_epoch();
        if checkpoint.source_epoch != source_epoch {
            return PackedByteArray::new();
        }
        let Some((authority, epoch)) = secure.expire_and_migrate(now_ms) else {
            return PackedByteArray::new();
        };
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        self.advance_gameplay_epoch(epoch);
        if authority.to_string() != self.local_player_id.to_string() {
            return PackedByteArray::new();
        }
        // The elected sender does not receive its own datagram, so install the
        // exact same state locally before advertising epoch 2.
        self.combat_authority = Some(restored_combat);
        self.combat_targets = Some(restored_targets);
        self.combat_receipts.extend(
            checkpoint
                .resolved_shots
                .iter()
                .map(|(shooter, tick)| (checkpoint.source_epoch, *shooter, *tick)),
        );
        let hash = checkpoint.hash();
        self.make_packet(
            i64::try_from(checkpoint.tick).unwrap_or(i64::MAX),
            PeerPayload::MigrationSnapshot {
                authority,
                epoch,
                checkpoint,
                state_hash: hash,
            },
        )
    }

    /// Build the exact combined M3 checkpoint without exposing private actor
    /// state as an input surface.
    #[func]
    fn m3_checkpoint_json(&self, combat_checkpoint_json: GString) -> GString {
        let Ok(combat) =
            serde_json::from_str::<MatchCheckpoint>(&combat_checkpoint_json.to_string())
        else {
            return GString::new();
        };
        let Some(checkpoint) = self.build_m3_checkpoint(combat) else {
            return GString::new();
        };
        GString::from(&serde_json::to_string(&checkpoint).unwrap_or_default())
    }

    /// Elect, preflight, install, fragment, and sign one complete M3 handoff.
    /// Non-successors return an empty array after adopting the elected epoch.
    #[func]
    fn poll_m3_migration(
        &mut self,
        now_ms: i64,
        combat_checkpoint_json: GString,
    ) -> Array<PackedByteArray> {
        let mut packets = Array::new();
        if !self.m3_wire_active {
            return packets;
        }
        let (Ok(now_ms), Ok(combat)) = (
            u64::try_from(now_ms),
            serde_json::from_str::<MatchCheckpoint>(&combat_checkpoint_json.to_string()),
        ) else {
            return packets;
        };
        let Some(checkpoint) = self.build_m3_checkpoint(combat) else {
            return packets;
        };
        let Some(next_epoch) = checkpoint.combat.source_epoch.checked_add(1) else {
            return packets;
        };
        let Some((restored, restored_bounty)) =
            Self::restore_m3_checkpoint(&checkpoint, next_epoch)
        else {
            return packets;
        };
        let Some(mut candidate_session) = self.m3_secure_session.clone() else {
            return packets;
        };
        let Some((authority, epoch)) = candidate_session.expire_and_migrate(now_ms) else {
            return packets;
        };
        let Ok(local) = PlayerId::parse(&self.local_player_id.to_string()) else {
            return packets;
        };
        if authority == local {
            let Ok(payloads) = fragment_m3_checkpoint(authority, epoch, &checkpoint) else {
                return Array::new();
            };
            let Some(seed) = self.session_key.as_ref() else {
                return Array::new();
            };
            let signing = SigningKey::from_bytes(seed);
            for payload in payloads {
                let Ok(envelope) =
                    candidate_session.envelope(checkpoint.combat.tick, payload, &signing)
                else {
                    return Array::new();
                };
                let Ok(encoded) = encode_m3(&envelope) else {
                    return Array::new();
                };
                packets.push(&PackedByteArray::from(encoded.as_slice()));
            }
        }
        self.m3_secure_session = Some(candidate_session);
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = i64::try_from(epoch).unwrap_or(i64::MAX);
        if authority == local {
            self.m3_combat_authority = Some(restored);
            self.m5_match_authority = Some(restored_bounty);
        }
        self.advance_gameplay_epoch(epoch);
        packets
    }

    /// Decode presentation fields only for explicit insecure diagnostics.
    #[func]
    fn decode_packet(&self, packet: PackedByteArray) -> VarDictionary {
        if self.m3_wire_active {
            return self.decode_m3_packet(packet);
        }
        let Ok(envelope) = decode(&packet.to_vec()) else {
            return VarDictionary::new();
        };
        let mut result = VarDictionary::new();
        result.set("sender", envelope.sender.to_string());
        result.set(
            "sequence",
            i64::try_from(envelope.sequence).unwrap_or(i64::MAX),
        );
        result.set(
            "tick",
            i64::try_from(envelope.simulation_tick).unwrap_or(i64::MAX),
        );
        result.set(
            "authority_epoch",
            i64::try_from(envelope.authority_epoch).unwrap_or(i64::MAX),
        );
        match envelope.payload {
            PeerPayload::Heartbeat => result.set("type", "heartbeat"),
            PeerPayload::Probe { nonce, reply } => {
                result.set("type", "probe");
                result.set("nonce", i64::try_from(nonce).unwrap_or(i64::MAX));
                result.set("reply", reply);
            }
            PeerPayload::Hello { hostname } => {
                result.set("type", "hello");
                result.set("hostname", hostname);
            }
            PeerPayload::RiderInput {
                throttle_milli,
                steer_milli,
                buttons,
            } => {
                result.set("type", "rider_input");
                result.set("throttle_milli", i64::from(throttle_milli));
                result.set("steer_milli", i64::from(steer_milli));
                result.set("buttons", i64::from(buttons));
            }
            PeerPayload::RiderSnapshot {
                rider_player_id,
                position_mm,
                velocity_mmps,
                yaw_millidegrees,
                stance,
            } => {
                result.set("type", "rider_snapshot");
                result.set("rider_player_id", rider_player_id.to_string());
                result.set(
                    "position",
                    Vector3::new(
                        position_mm[0] as f32 / 1_000.0,
                        position_mm[1] as f32 / 1_000.0,
                        position_mm[2] as f32 / 1_000.0,
                    ),
                );
                result.set(
                    "velocity",
                    Vector3::new(
                        velocity_mmps[0] as f32 / 1_000.0,
                        velocity_mmps[1] as f32 / 1_000.0,
                        velocity_mmps[2] as f32 / 1_000.0,
                    ),
                );
                result.set("yaw_degrees", f64::from(yaw_millidegrees) / 1_000.0);
                result.set("stance_id", i64::from(stance.as_u8()));
                result.set("stance_known", stance.is_known());
            }
            PeerPayload::ShotCommand { command } => {
                result.set("type", "shot_command");
                result.set("shooter_player_id", command.shooter_peer_id.to_string());
                result.set(
                    "command_json",
                    serde_json::to_string(&command).unwrap_or_default(),
                );
            }
            PeerPayload::ShotResult {
                result: shot_result,
            } => {
                result.set("type", "shot_result");
                result.set("shooter_player_id", shot_result.shooter_peer_id.to_string());
                result.set(
                    "result_json",
                    serde_json::to_string(&shot_result).unwrap_or_default(),
                );
            }
            PeerPayload::Authority { authority, epoch } => {
                result.set("type", "authority");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
            }
            PeerPayload::MigrationSnapshot {
                authority,
                epoch,
                checkpoint,
                state_hash,
            } => {
                result.set("type", "migration_snapshot");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
                result.set(
                    "snapshot_tick",
                    i64::try_from(checkpoint.tick).unwrap_or(i64::MAX),
                );
                result.set(
                    "checkpoint_json",
                    serde_json::to_string(&checkpoint).unwrap_or_default(),
                );
                result.set("state_hash", hex_bytes(&state_hash));
            }
            PeerPayload::Leave => result.set("type", "leave"),
        }
        result
    }

    fn decode_m3_packet(&self, packet: PackedByteArray) -> VarDictionary {
        let Ok(envelope) = decode_m3(&packet.to_vec()) else {
            return VarDictionary::new();
        };
        let mut result = VarDictionary::new();
        result.set("sender", envelope.sender.to_string());
        result.set(
            "sequence",
            i64::try_from(envelope.sequence).unwrap_or(i64::MAX),
        );
        result.set(
            "tick",
            i64::try_from(envelope.simulation_tick).unwrap_or(i64::MAX),
        );
        result.set(
            "authority_epoch",
            i64::try_from(envelope.authority_epoch).unwrap_or(i64::MAX),
        );
        match envelope.payload {
            M3PeerPayloadV2::Hello { hostname } => {
                result.set("type", "hello");
                result.set("hostname", hostname);
            }
            M3PeerPayloadV2::Heartbeat => result.set("type", "heartbeat"),
            M3PeerPayloadV2::Probe { nonce, reply } => {
                result.set("type", "probe");
                result.set("nonce", i64::try_from(nonce).unwrap_or(i64::MAX));
                result.set("reply", reply);
            }
            M3PeerPayloadV2::ActorLoadout { loadout } => {
                result.set("type", "actor_loadout");
                result.set(
                    "loadout_json",
                    serde_json::to_string(&loadout).unwrap_or_default(),
                );
            }
            M3PeerPayloadV2::ActorInput { input } => {
                result.set("type", "actor_input");
                result.set(
                    "input_json",
                    serde_json::to_string(&input).unwrap_or_default(),
                );
            }
            M3PeerPayloadV2::ActorSnapshot { snapshot } => {
                result.set("type", "actor_snapshot");
                result.set("rider_player_id", snapshot.rider_player_id.to_string());
                result.set(
                    "snapshot_json",
                    serde_json::to_string(&snapshot).unwrap_or_default(),
                );
            }
            M3PeerPayloadV2::ShotCommand { command } => {
                result.set("type", "shot_command");
                result.set("shooter_player_id", command.shooter_peer_id.to_string());
                result.set(
                    "command_json",
                    serde_json::to_string(&command).unwrap_or_default(),
                );
            }
            M3PeerPayloadV2::ShotResult {
                result: shot_result,
            } => {
                result.set("type", "shot_result");
                result.set("shooter_player_id", shot_result.shooter_peer_id.to_string());
                result.set(
                    "result_json",
                    serde_json::to_string(&shot_result).unwrap_or_default(),
                );
            }
            M3PeerPayloadV2::Authority { authority, epoch } => {
                result.set("type", "authority");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
            }
            M3PeerPayloadV2::MigrationFragment {
                authority,
                epoch,
                state_hash,
                fragment_index,
                fragment_count,
                ..
            } => {
                result.set("type", "migration_fragment");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
                result.set("state_hash", hex_bytes(&state_hash));
                result.set("fragment_index", i64::from(fragment_index));
                result.set("fragment_count", i64::from(fragment_count));
            }
            M3PeerPayloadV2::Leave => result.set("type", "leave"),
        }
        result
    }

    /// Legacy packet acceptance, available only after explicit demo/test opt-in.
    #[func]
    fn accept_packet(&mut self, packet: PackedByteArray, now_ms: i64) -> i64 {
        if self.m3_wire_active || !self.insecure_demo_mode || self.secure_session.is_some() {
            return -1;
        }
        let (Some(session), Ok(envelope), Ok(now_ms)) = (
            self.session.as_mut(),
            decode(&packet.to_vec()),
            u64::try_from(now_ms),
        ) else {
            return -1;
        };
        let outcome = session.accept(&envelope, now_ms);
        self.authority_player_id = GString::from(&session.authority().to_string());
        self.authority_epoch = i64::try_from(session.authority_epoch()).unwrap_or(i64::MAX);
        Self::outcome_code(outcome)
    }

    /// Single native secure receive gate. Source and current node identity are
    /// verified before replay, epoch, or authority state can mutate.
    #[func]
    fn accept_packet_with_source(
        &mut self,
        packet: PackedByteArray,
        source_ip: GString,
        source_port: i64,
        source_node_key: GString,
        now_ms: i64,
    ) -> i64 {
        if self.m3_wire_active {
            return self.accept_m3_packet_with_source(
                packet,
                source_ip,
                source_port,
                source_node_key,
                now_ms,
            );
        }
        let (Some(session), Ok(envelope), Ok(source_ip), Ok(source_port), Ok(now_ms)) = (
            self.secure_session.as_mut(),
            decode(&packet.to_vec()),
            source_ip.to_string().parse::<IpAddr>(),
            u16::try_from(source_port),
            u64::try_from(now_ms),
        ) else {
            return -1;
        };
        let node_key = if source_node_key.is_empty() {
            None
        } else {
            match NodeKey::parse(&source_node_key.to_string()) {
                Ok(key) => Some(key),
                Err(_) => return -1,
            }
        };
        let outcome = session.accept_with_source(
            &envelope,
            SocketAddr::new(source_ip, source_port),
            node_key,
            now_ms,
        );
        let accepted_migration = if outcome == AcceptOutcome::Accepted {
            if let PeerPayload::MigrationSnapshot {
                checkpoint, epoch, ..
            } = &envelope.payload
            {
                if let Some((restored_combat, restored_targets)) =
                    Self::restore_combat_checkpoint(checkpoint, *epoch)
                {
                    self.combat_authority = Some(restored_combat);
                    self.combat_targets = Some(restored_targets);
                    self.combat_receipts.extend(
                        checkpoint
                            .resolved_shots
                            .iter()
                            .map(|(shooter, tick)| (checkpoint.source_epoch, *shooter, *tick)),
                    );
                    Some(*epoch)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let authority = session.state().authority();
        let authority_epoch = session.state().authority_epoch();
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        if let Some(epoch) = accepted_migration {
            self.advance_gameplay_epoch(epoch);
        }
        Self::outcome_code(outcome)
    }

    fn accept_m3_packet_with_source(
        &mut self,
        packet: PackedByteArray,
        source_ip: GString,
        source_port: i64,
        source_node_key: GString,
        now_ms: i64,
    ) -> i64 {
        let (Ok(envelope), Ok(source_ip), Ok(source_port), Ok(now_ms)) = (
            decode_m3(&packet.to_vec()),
            source_ip.to_string().parse::<IpAddr>(),
            u16::try_from(source_port),
            u64::try_from(now_ms),
        ) else {
            return -1;
        };
        let node_key = if source_node_key.is_empty() {
            None
        } else {
            match NodeKey::parse(&source_node_key.to_string()) {
                Ok(key) => Some(key),
                Err(_) => return -1,
            }
        };
        if matches!(
            &envelope.payload,
            M3PeerPayloadV2::ActorLoadout { loadout }
                if !self.m3_loadout_matches(envelope.sender, *loadout)
        ) {
            return Self::outcome_code(AcceptOutcome::InvalidPayloadSubject);
        }
        let mut prepared_authority = None;
        let (outcome, authority, authority_epoch, installed) = {
            let Some(session) = self.m3_secure_session.as_mut() else {
                return -1;
            };
            let outcome = session.accept_with_source_validated(
                &envelope,
                SocketAddr::new(source_ip, source_port),
                node_key,
                now_ms,
                |checkpoint| {
                    prepared_authority =
                        Self::restore_m3_checkpoint(checkpoint, envelope.authority_epoch);
                    prepared_authority.is_some()
                },
            );
            let installed = if outcome == AcceptOutcome::Accepted {
                session.take_installed_checkpoint()
            } else {
                None
            };
            (
                outcome,
                session.state().authority(),
                session.state().authority_epoch(),
                installed,
            )
        };
        let migrated = installed.is_some();
        if migrated {
            let Some((authority, bounty)) = prepared_authority else {
                return Self::outcome_code(AcceptOutcome::InvalidCheckpoint);
            };
            self.m3_combat_authority = Some(authority);
            self.m5_match_authority = Some(bounty);
        }
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = i64::try_from(authority_epoch).unwrap_or(i64::MAX);
        if migrated {
            self.advance_gameplay_epoch(authority_epoch);
        }
        Self::outcome_code(outcome)
    }

    /// Atomically admit and dispatch one immutable packet. Rejected traffic
    /// never exposes a gameplay payload to GDScript.
    #[func]
    fn dispatch_packet_with_source(
        &mut self,
        packet: PackedByteArray,
        source_ip: GString,
        source_port: i64,
        source_node_key: GString,
        now_ms: i64,
    ) -> VarDictionary {
        let outcome = self.accept_packet_with_source(
            packet.clone(),
            source_ip,
            source_port,
            source_node_key,
            now_ms,
        );
        let mut result = VarDictionary::new();
        result.set("accepted", outcome == 0);
        result.set("outcome", outcome);
        if outcome != 0 {
            return result;
        }
        let payload = self.decode_packet(packet);
        for (key, value) in payload.iter_shared() {
            result.set(&key, &value);
        }
        result
    }

    /// Bind the public local player subject used by native lobby requests.
    #[func]
    fn configure_lobby_player(&mut self, player_id: GString) -> bool {
        self.lobby_client.configure_player(&player_id.to_string())
    }

    #[func]
    fn probe_lobby_readiness(&self) {
        self.lobby_client.request_public(
            LobbyOperation::Readiness,
            Method::GET,
            route_for(LobbyOperation::Readiness, ""),
            None,
            false,
            false,
        );
    }

    #[func]
    fn capture_launch_code(&self) {
        if let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/LaunchCode") {
            input.bind_mut().arm_capture();
        }
    }

    #[func]
    fn capture_join_code(&self) {
        if let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/JoinCode") {
            input.bind_mut().arm_capture();
        }
    }

    #[func]
    fn submit_create(&self, display_name: GString) {
        let Some(name) = Self::clean_public_name(&display_name) else {
            return;
        };
        let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/LaunchCode")
        else {
            return;
        };
        match input.bind_mut().consume() {
            Ok(grant) => self.lobby_client.create(&name, grant),
            Err(_) => self
                .lobby_client
                .fail_now(LobbyOperation::Create, NativeLobbyError::Secret),
        };
    }

    #[func]
    fn submit_join(&self, display_name: GString) {
        let Some(name) = Self::clean_public_name(&display_name) else {
            return;
        };
        let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/JoinCode")
        else {
            return;
        };
        match input.bind_mut().consume_join_code() {
            Ok((lobby_id, invitation)) => self.lobby_client.join(&lobby_id, &name, invitation),
            Err(_) => self
                .lobby_client
                .fail_now(LobbyOperation::Join, NativeLobbyError::Secret),
        };
    }

    #[func]
    fn auto_join_creator(&mut self, lobby_id: GString, display_name: GString) {
        let Some(name) = Self::clean_public_name(&display_name) else {
            return;
        };
        let lobby = lobby_id.to_string();
        self.creator_join_display = Some(name);
        self.creator_join_lobby = Some(lobby.clone());
        self.lobby_client.invitation(&lobby, true);
    }

    #[func]
    fn copy_invitation_to_clipboard(&self, lobby_id: GString) {
        self.lobby_client.invitation(&lobby_id.to_string(), false);
    }

    #[func]
    fn has_creator_control(&self) -> bool {
        self.lobby_client.has_creator()
    }

    #[func]
    fn has_participant_access(&self) -> bool {
        self.lobby_client.has_participant()
    }

    #[func]
    fn poll_lobby(&self, lobby_id: GString) {
        let id = lobby_id.to_string();
        self.lobby_client.request_public(
            LobbyOperation::Lobby,
            Method::GET,
            route_for(LobbyOperation::Lobby, &id),
            None,
            false,
            false,
        );
    }

    #[func]
    fn poll_network(&self, lobby_id: GString) {
        let id = lobby_id.to_string();
        self.lobby_client.request_public(
            LobbyOperation::Network,
            Method::GET,
            route_for(LobbyOperation::Network, &id),
            None,
            false,
            false,
        );
    }

    #[func]
    #[allow(clippy::too_many_arguments)]
    fn register_endpoint(
        &mut self,
        lobby_id: GString,
        network_generation: i64,
        roster_revision: i64,
        address: GString,
        port: i64,
        session_public_key: GString,
        key_proof: GString,
    ) {
        let (Ok(network_generation), Ok(roster_revision), Ok(port)) = (
            u64::try_from(network_generation),
            u64::try_from(roster_revision),
            u16::try_from(port),
        ) else {
            return;
        };
        self.lobby_client.last_endpoint_sequence = self
            .lobby_client
            .last_endpoint_sequence
            .saturating_add(1)
            .max(unix_millis());
        let body = serde_json::json!({
            "network_generation": network_generation,
            "roster_revision": roster_revision,
            "sequence": self.lobby_client.last_endpoint_sequence,
            "tailnet_address": address.to_string(),
            "application_port": port,
            "session_public_key": session_public_key.to_string(),
            "key_proof": key_proof.to_string(),
        });
        let id = lobby_id.to_string();
        self.lobby_client.request_public(
            LobbyOperation::Endpoint,
            Method::POST,
            route_for(LobbyOperation::Endpoint, &id),
            Some(body.to_string()),
            false,
            true,
        );
    }

    #[func]
    fn submit_measurements(&self, lobby_id: GString, report_json: GString) {
        let (Ok(mut body), Some(player)) = (
            serde_json::from_str::<serde_json::Value>(&report_json.to_string()),
            self.lobby_client.player_id(),
        ) else {
            return;
        };
        if let serde_json::Value::Object(map) = &mut body {
            map.insert(
                "player_id".into(),
                serde_json::Value::String(player.to_owned()),
            );
        }
        let id = lobby_id.to_string();
        self.lobby_client.request_public(
            LobbyOperation::Report,
            Method::POST,
            route_for(LobbyOperation::Report, &id),
            Some(body.to_string()),
            false,
            false,
        );
    }

    #[func]
    fn start_lobby(&self, lobby_id: GString) {
        let id = lobby_id.to_string();
        let body = serde_json::json!({"creator_player_id": self.lobby_client.player_id().unwrap_or_default()}).to_string();
        self.lobby_client.request_public(
            LobbyOperation::Start,
            Method::POST,
            route_for(LobbyOperation::Start, &id),
            Some(body),
            true,
            true,
        );
    }

    #[func]
    fn authority_heartbeat(&self, lobby_id: GString, input_hash: GString) {
        if input_hash.len() != 64 {
            return;
        }
        let id = lobby_id.to_string();
        let body = serde_json::json!({"player_id": self.lobby_client.player_id().unwrap_or_default(), "input_hash": input_hash.to_string()}).to_string();
        self.lobby_client.request_public(
            LobbyOperation::Heartbeat,
            Method::POST,
            route_for(LobbyOperation::Heartbeat, &id),
            Some(body),
            false,
            false,
        );
    }

    #[func]
    fn leave_lobby(&self, lobby_id: GString) {
        let id = lobby_id.to_string();
        let body =
            serde_json::json!({"player_id": self.lobby_client.player_id().unwrap_or_default()})
                .to_string();
        self.lobby_client.request_public(
            LobbyOperation::Leave,
            Method::POST,
            route_for(LobbyOperation::Leave, &id),
            Some(body),
            false,
            true,
        );
    }

    #[func]
    fn end_lobby(&self, lobby_id: GString) {
        let id = lobby_id.to_string();
        self.lobby_client.request_public(
            LobbyOperation::End,
            Method::DELETE,
            route_for(LobbyOperation::End, &id),
            None,
            true,
            true,
        );
    }

    #[func]
    fn cancel_lobby_operations(&mut self) {
        self.lobby_client.cancel();
        self.creator_join_display = None;
        self.creator_join_lobby = None;
        self.shutdown();
        if let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/LaunchCode") {
            input.bind_mut().clear_capture();
        }
        if let Some(mut input) = self.secret_input("../Screens/Title/Card/Margin/VBox/JoinCode") {
            input.bind_mut().clear_capture();
        }
    }

    /// Explicit local-demo enrollment. The file path and bytes are read and
    /// removed entirely in Rust; no bearer value crosses the Godot ABI.
    #[func]
    fn connect_demo_peer(&mut self, hostname: GString, port: i64) -> bool {
        let Ok(port) = u16::try_from(port) else {
            return false;
        };
        let Some(path) = std::env::var_os("SPURFIRE_P2P_DEMO_KEY_FILE") else {
            return false;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        let mut enrollment = Zeroizing::new(bytes);
        while enrollment.last().is_some_and(u8::is_ascii_whitespace) {
            enrollment.pop();
        }
        while enrollment.first().is_some_and(u8::is_ascii_whitespace) {
            enrollment.remove(0);
        }
        if enrollment.is_empty() {
            return false;
        }
        let started = self.connect_native(hostname.to_string(), enrollment, port);
        if started {
            let _ = std::fs::remove_file(path);
        }
        started
    }

    /// Send one bounded Spurfire envelope returned by the protocol codec.
    #[func]
    fn send_packet(&mut self, packet: PackedByteArray, destination_ip: GString, port: i64) -> bool {
        let bytes = packet.to_vec();
        let valid = if self.m3_wire_active {
            decode_m3(&bytes).is_ok()
        } else {
            decode(&bytes).is_ok()
        };
        if !valid {
            return false;
        }
        let Some(destination) = parse_destination(&destination_ip.to_string(), port) else {
            return false;
        };
        let Some(sender) = &self.command_tx else {
            return false;
        };
        sender
            .send(WorkerCommand::Send {
                packet: bytes,
                destination,
            })
            .is_ok()
    }

    /// Request RustScale's current path classification for one peer IP.
    #[func]
    fn query_route(&mut self, peer_ip: GString) -> bool {
        let Ok(peer_ip) = peer_ip.to_string().parse::<IpAddr>() else {
            return false;
        };
        self.command_tx
            .as_ref()
            .is_some_and(|sender| sender.send(WorkerCommand::QueryRoute { peer_ip }).is_ok())
    }

    /// Build a sequenced peer Leave packet before transport shutdown.
    /// Real admission remains closed until envelopes are cryptographically bound.
    #[func]
    fn make_leave(&mut self, tick: i64) -> PackedByteArray {
        if self.m3_wire_active {
            self.make_m3_packet(tick, M3PeerPayloadV2::Leave)
        } else {
            self.make_packet(tick, PeerPayload::Leave)
        }
    }

    /// Cancel in-flight enrollment or stop live traffic, whichever is active.
    #[func]
    fn shutdown(&mut self) {
        if let Some(sender) = self.command_tx.take() {
            // The worker selects on this command even while enrollment is
            // still blocked on the RustScale control server.
            let _ = sender.send(WorkerCommand::Stop);
            self.connection_state = "disconnecting".into();
        }
    }

    fn join_worker(&mut self) {
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.join();
        }
    }

    /// Forget all lobby-scoped identity after leave. Enrollment material is
    /// owned by the worker and dropped when shutdown completes.
    #[func]
    fn clear_lobby_session(&mut self) {
        self.shutdown();
        self.join_worker();
        // Invalidate every event the orphaned worker can still emit so a late
        // `connected` from a cancelled enrollment cannot resurrect state.
        self.worker_generation = self.worker_generation.wrapping_add(1);
        self.connection_state = "offline".into();
        self.session = None;
        self.secure_session = None;
        self.m3_wire_active = false;
        self.m3_secure_session = None;
        self.m3_loadouts.clear();
        self.session_key = None;
        self.session_key_generation = 0;
        self.pinned_manifest_key = None;
        self.allowed_players.clear();
        self.local_player_id = GString::new();
        self.authority_player_id = GString::new();
        self.authority_epoch = 0;
        self.tailnet_ip = GString::new();
        self.local_port = 0;
        self.combat_authority = None;
        self.combat_targets = None;
        self.authority_rider_history.clear();
        self.combat_receipts.clear();
        self.m3_combat_authority = None;
        self.m5_match_authority = None;
    }

    fn make_packet(&mut self, tick: i64, payload: PeerPayload) -> PackedByteArray {
        if self.m3_wire_active {
            return PackedByteArray::new();
        }
        let Ok(tick) = u64::try_from(tick) else {
            return PackedByteArray::new();
        };
        let envelope = if let (Some(secure), Some(seed)) =
            (self.secure_session.as_mut(), self.session_key.as_ref())
        {
            let signing = SigningKey::from_bytes(seed);
            secure.envelope(tick, payload, &signing).ok()
        } else if self.insecure_demo_mode {
            self.session
                .as_mut()
                .map(|session| session.envelope(tick, payload))
        } else {
            None
        };
        envelope
            .and_then(|envelope| encode(&envelope).ok())
            .map_or_else(PackedByteArray::new, |bytes| {
                PackedByteArray::from(bytes.as_slice())
            })
    }

    fn make_m3_packet(&mut self, tick: i64, payload: M3PeerPayloadV2) -> PackedByteArray {
        if !self.m3_wire_active {
            return PackedByteArray::new();
        }
        let (Ok(tick), Some(secure), Some(seed)) = (
            u64::try_from(tick),
            self.m3_secure_session.as_mut(),
            self.session_key.as_ref(),
        ) else {
            return PackedByteArray::new();
        };
        let signing = SigningKey::from_bytes(seed);
        secure
            .envelope(tick, payload, &signing)
            .ok()
            .and_then(|envelope| encode_m3(&envelope).ok())
            .map_or_else(PackedByteArray::new, |bytes| {
                PackedByteArray::from(bytes.as_slice())
            })
    }

    fn advance_gameplay_epoch(&mut self, authority_epoch: u64) {
        if let Some(mut rider) = self
            .base()
            .try_get_node_as::<SaddleDiveController>(&self.gameplay_rider_path)
        {
            let _ = rider.bind_mut().advance_authority_epoch(authority_epoch);
        }
    }

    fn restore_combat_checkpoint(
        checkpoint: &MatchCheckpoint,
        authority_epoch: u64,
    ) -> Option<(CombatAuthority, TargetRegistry)> {
        let mut restored_combat = CombatAuthority::new(60, 0).ok()?;
        if !restored_combat.set_authority_epoch(authority_epoch) {
            return None;
        }
        let mut restored_targets = TargetRegistry::new(60).ok()?;
        for rider in &checkpoint.riders {
            let weapon = WeaponId::try_from(i64::from(rider.weapon_id)).ok()?;
            if !restored_combat.restore_shooter(
                rider.rider_player_id,
                weapon,
                WeaponAmmo {
                    magazine: rider.ammo_magazine,
                    reserve: rider.ammo_reserve,
                },
                rider.last_command_tick.map(SimulationTick::new),
                rider.last_shot_tick.map(SimulationTick::new),
                rider.shot_index,
            ) {
                return None;
            }
            restored_targets
                .restore(rider_target_definition(rider.rider_player_id), rider.health)
                .ok()?;
        }
        Some((restored_combat, restored_targets))
    }

    fn build_m3_checkpoint(&self, combat: MatchCheckpoint) -> Option<M3MatchCheckpointV2> {
        let authority = self.m3_combat_authority.as_ref()?;
        let bounty = self.m5_match_authority.as_ref()?;
        let checkpoint = M3MatchCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            combat,
            gameplay: authority.actors().checkpoint(),
            reloads: authority.reload_checkpoints(),
            next_horse_damage_sequence: authority.next_horse_damage_sequence(),
            bounty: bounty.checkpoint(),
        };
        checkpoint.is_bounded_and_canonical().then_some(checkpoint)
    }

    fn m3_loadout_matches(&self, player: PlayerId, loadout: M3ActorLoadout) -> bool {
        self.m3_combat_authority.as_ref().is_some_and(|authority| {
            authority
                .actor(player)
                .is_some_and(|actor| actor.horse().max_health() == loadout.horse_class.max_health())
                && authority
                    .combat()
                    .shooter_kernel(player)
                    .is_some_and(|kernel| kernel.equipped_weapon() == loadout.weapon_id)
        })
    }

    fn restore_m3_checkpoint(
        checkpoint: &M3MatchCheckpointV2,
        authority_epoch: u64,
    ) -> Option<(M3CombatAuthority, BountyMatchKernel)> {
        if !checkpoint.is_bounded_and_canonical()
            || checkpoint.combat.source_epoch.checked_add(1) != Some(authority_epoch)
        {
            return None;
        }
        let (combat, mut targets) =
            Self::restore_combat_checkpoint(&checkpoint.combat, authority_epoch)?;
        let actors =
            M3AuthorityBank::restore_checkpoint(checkpoint.gameplay.clone(), authority_epoch)
                .ok()?;
        let mut rider_entities = BTreeMap::new();
        for row in actors.checkpoint().actors() {
            let player = row.rider_player_id;
            if row.horse_entity_id != horse_entity_id(player)
                || rider_entities
                    .insert(player, rider_entity_id(player))
                    .is_some()
            {
                return None;
            }
            let actor = actors.actor(player)?;
            targets
                .restore(
                    TargetDefinition {
                        entity_id: row.horse_entity_id,
                        owner_peer_id: Some(player),
                        team_id: TeamId(1),
                        max_health: actor.horse().max_health(),
                    },
                    actor.horse().health(),
                )
                .ok()?;
        }
        let combat = M3CombatAuthority::restore_components(
            combat,
            targets,
            actors,
            rider_entities,
            checkpoint.reloads.clone(),
            checkpoint.next_horse_damage_sequence,
        )
        .ok()?;
        let bounty =
            BountyMatchKernel::restore_checkpoint(checkpoint.bounty.clone(), authority_epoch)
                .ok()?;
        Some((combat, bounty))
    }

    const fn outcome_code(outcome: AcceptOutcome) -> i64 {
        match outcome {
            AcceptOutcome::Accepted => 0,
            AcceptOutcome::DuplicateOrReplay => 1,
            AcceptOutcome::WrongLobby => 2,
            AcceptOutcome::StaleAuthorityEpoch => 3,
            AcceptOutcome::UnknownSender => 4,
            AcceptOutcome::UnsignedInSecureMode => 5,
            AcceptOutcome::EndpointMismatch => 6,
            AcceptOutcome::NodeKeyMismatch => 7,
            AcceptOutcome::WrongGeneration => 8,
            AcceptOutcome::RosterMismatch => 9,
            AcceptOutcome::BadSignature => 10,
            AcceptOutcome::InvalidAuthorityClaim => 11,
            AcceptOutcome::InvalidPayloadRole => 12,
            AcceptOutcome::InvalidPayloadSubject => 13,
            AcceptOutcome::InvalidCheckpoint => 14,
            AcceptOutcome::DuplicateShotResult => 15,
            AcceptOutcome::PendingMigration => 16,
        }
    }

    fn poll_events(&mut self) {
        let Some(receiver) = self.event_rx.take() else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok((generation, event)) => {
                    // A worker from a cleared or replaced session must never
                    // mutate state or re-emit signals.
                    if generation != self.worker_generation {
                        continue;
                    }
                    match event {
                        WorkerEvent::Connected { ip, port } => {
                            self.connection_state = "connected".into();
                            self.tailnet_ip = GString::from(&ip);
                            self.local_port = i64::from(port);
                            let signal_ip = GString::from(&ip);
                            self.signals().connected().emit(&signal_ip, i64::from(port));
                        }
                        WorkerEvent::Packet {
                            packet,
                            source,
                            node_key,
                        } => {
                            let signal_packet = PackedByteArray::from(packet.as_slice());
                            let source_ip = source.ip().to_string();
                            let signal_ip = GString::from(&source_ip);
                            let signal_node = GString::from(
                                &node_key.map_or_else(String::new, |key| key.to_string()),
                            );
                            self.signals().packet_received().emit(
                                &signal_packet,
                                &signal_ip,
                                i64::from(source.port()),
                                &signal_node,
                            );
                        }
                        WorkerEvent::Route { peer_ip, route } => {
                            let signal_ip = GString::from(&peer_ip);
                            let signal_route = GString::from(&route);
                            self.signals()
                                .route_updated()
                                .emit(&signal_ip, &signal_route);
                        }
                        WorkerEvent::Failed(message) => {
                            self.connection_state = "error".into();
                            self.command_tx = None;
                            let signal_message = GString::from(&message);
                            self.signals().connection_failed().emit(&signal_message);
                        }
                        WorkerEvent::Stopped => {
                            self.connection_state = "offline".into();
                            self.command_tx = None;
                            self.signals().disconnected().emit();
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.command_tx = None;
                    break;
                }
            }
        }
        self.event_rx = Some(receiver);
        if self
            .worker_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            self.join_worker();
        }
    }
}

#[godot_api]
impl INode for PeerSession {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            gameplay_rider_path: NodePath::from("../Rider"),
            connection_state: "offline".into(),
            tailnet_ip: GString::new(),
            local_port: 0,
            local_player_id: GString::new(),
            authority_player_id: GString::new(),
            authority_epoch: 0,
            session: None,
            secure_session: None,
            m3_wire_active: false,
            m3_secure_session: None,
            m3_loadouts: BTreeMap::new(),
            session_key: None,
            session_key_generation: 0,
            pinned_manifest_key: None,
            insecure_demo_mode: false,
            allowed_players: BTreeSet::new(),
            command_tx: None,
            event_rx: None,
            worker_handle: None,
            worker_generation: 0,
            combat_authority: None,
            combat_targets: None,
            authority_rider_history: BTreeMap::new(),
            combat_receipts: BTreeSet::new(),
            m3_combat_authority: None,
            m5_match_authority: None,
            lobby_client: LobbyClientState::default(),
            creator_join_display: None,
            creator_join_lobby: None,
        }
    }

    fn process(&mut self, _delta: f64) {
        self.poll_events();
        self.poll_lobby_events();
    }
    fn exit_tree(&mut self) {
        self.lobby_client.cancel();
        self.shutdown();
        self.join_worker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spurfire_net::RiderCheckpoint;
    use spurfire_protocol::{QuantizedDirection, ShotRejectionReason};

    fn test_player(value: u8) -> PlayerId {
        PlayerId::parse(&format!("00000000-0000-4000-8000-{value:012}")).expect("fixture player")
    }

    #[test]
    fn airborne_authority_snapshot_requires_and_retains_dive_identity() {
        let dive = snapshot_dive_id(RiderStance::SaddleDiveAirborne, 7)
            .expect("valid airborne identity")
            .expect("airborne dive");
        assert_eq!(dive.get(), 7);
        assert_eq!(snapshot_dive_id(RiderStance::SaddleDiveAirborne, -1), None);
        assert_eq!(snapshot_dive_id(RiderStance::Mounted, -1), Some(None));
        assert_eq!(snapshot_dive_id(RiderStance::Mounted, 7), None);
    }

    #[test]
    fn authority_resolution_never_trusts_command_weapon_origin_or_riding_state() {
        let shooter = test_player(1);
        let mut authority = CombatAuthority::new(60, 0).unwrap();
        assert!(authority.register_shooter(shooter, WeaponId::Dustwalker));
        let mut targets = TargetRegistry::new(60).unwrap();
        let origin = QuantizedOrigin::from_meters(0.0, 0.0, 0.0).unwrap();
        let direction = QuantizedDirection::from_components(0.0, 0.0, -1.0).unwrap();
        let snapshot = |tick, stance, muzzle_origin| CombatRiderSnapshot {
            tick: SimulationTick::new(tick),
            shooter_peer_id: shooter,
            muzzle_origin,
            team_id: TeamId::default(),
            riding: RidingState {
                stance,
                ..RidingState::default()
            },
        };
        let mut history = BTreeMap::from([(
            shooter,
            VecDeque::from([snapshot(1, RiderStance::Mounted, origin)]),
        )]);
        let command = |tick, weapon_id, command_origin, spread_seed| ShotCommand {
            tick: SimulationTick::new(tick),
            shooter_peer_id: shooter,
            weapon_id,
            origin: command_origin,
            direction,
            spread_seed,
            claimed_target: None,
        };

        let seed = authority.expected_spread_seed(shooter).unwrap();
        let forged_weapon = resolve_authority_shot(
            &mut authority,
            &mut targets,
            &history,
            &command(1, WeaponId::Longspur, origin, seed),
        )
        .unwrap();
        assert_eq!(forged_weapon.weapon_id, WeaponId::Dustwalker);
        assert_eq!(
            forged_weapon.rejection_reason,
            Some(ShotRejectionReason::Weapon)
        );

        let far_origin = QuantizedOrigin::from_meters(20.0, 0.0, 0.0).unwrap();
        history
            .get_mut(&shooter)
            .unwrap()
            .push_back(snapshot(2, RiderStance::Mounted, origin));
        let forged_origin = resolve_authority_shot(
            &mut authority,
            &mut targets,
            &history,
            &command(2, WeaponId::Dustwalker, far_origin, seed),
        )
        .unwrap();
        assert_eq!(
            forged_origin.rejection_reason,
            Some(ShotRejectionReason::OriginLeash)
        );

        history.get_mut(&shooter).unwrap().push_back(snapshot(
            3,
            RiderStance::OnFootStanding,
            origin,
        ));
        let dismounted = resolve_authority_shot(
            &mut authority,
            &mut targets,
            &history,
            &command(3, WeaponId::Dustwalker, origin, seed),
        )
        .unwrap();
        assert_eq!(
            dismounted.rejection_reason,
            Some(ShotRejectionReason::Dismounted)
        );
    }

    #[test]
    fn combat_checkpoint_restores_ammo_ticks_index_and_receipts() {
        let shooter = test_player(1);
        let checkpoint = MatchCheckpoint {
            source_epoch: 1,
            tick: 30,
            riders: vec![RiderCheckpoint {
                rider_player_id: shooter,
                position_mm: [1, 2, 3],
                velocity_mmps: [4, 5, 6],
                yaw_millidegrees: 7,
                stance: RiderStance::Mounted,
                health: 82,
                weapon_id: WeaponId::Longspur.as_u8(),
                ammo_magazine: 2,
                ammo_reserve: 9,
                last_input_tick: 29,
                last_shot_tick: Some(24),
                last_command_tick: Some(25),
                shot_index: 4,
            }],
            resolved_shots: vec![(shooter, 24)],
        };
        let (restored, mut targets) =
            PeerSession::restore_combat_checkpoint(&checkpoint, 2).unwrap();
        assert_eq!(restored.authority_epoch(), 2);
        let kernel = restored.shooter_kernel(shooter).unwrap();
        assert_eq!(kernel.equipped_weapon(), WeaponId::Longspur);
        assert_eq!(
            kernel.equipped_ammo(),
            WeaponAmmo {
                magazine: 2,
                reserve: 9
            }
        );
        assert_eq!(kernel.last_accepted_tick(), Some(SimulationTick::new(24)));
        assert_eq!(
            restored.last_command_tick(shooter),
            Some(SimulationTick::new(25))
        );
        assert_eq!(kernel.shot_index(), 4);
        let target_id = rider_entity_id(shooter);
        assert_eq!(targets.health(target_id), Some(82));
        let center = QuantizedOrigin::from_meters(1.0, 2.0, 3.0).unwrap();
        assert!(targets
            .record_pose(TargetPoseSnapshot {
                tick: SimulationTick::new(31),
                entity_id: target_id,
                stance: RiderStance::Mounted,
                body_center: center,
                body_half_height_mm: 500,
                body_radius_mm: 350,
                head_center: center,
                head_radius_mm: 250,
                active: true,
            })
            .is_ok());
        assert_eq!(targets.health(target_id), Some(82));
    }

    #[test]
    fn m3_checkpoint_restore_preflights_deterministic_horse_target_graph() {
        let players = [test_player(1), test_player(2)];
        let mut authority = M3CombatAuthority::new(60, 0, 1).unwrap();
        for player in players {
            assert!(authority.register_actor(
                player,
                rider_entity_id(player),
                horse_entity_id(player),
                HorseVitalityClass::Courser,
                WeaponId::Dustwalker,
                TeamId(1),
            ));
        }
        let tick = SimulationTick::new(10);
        for player in players {
            authority
                .advance_actor(
                    player,
                    ActorM3TickInput {
                        tick,
                        on_foot: OnFootTickInput {
                            tick,
                            move_direction: None,
                            sprint_pressed: false,
                            crouch_pressed: false,
                            reload_active: false,
                        },
                        interact_pressed: false,
                        spur_pressed: false,
                        mounted_for_spur: true,
                        rider_position: QuantizedOrigin::default(),
                        return_horse_position: QuantizedOrigin::default(),
                        return_horse_moving: false,
                    },
                )
                .unwrap();
        }
        let rider = |rider_player_id| RiderCheckpoint {
            rider_player_id,
            position_mm: [0; 3],
            velocity_mmps: [0; 3],
            yaw_millidegrees: 0,
            stance: RiderStance::Mounted,
            health: 100,
            weapon_id: WeaponId::Dustwalker.as_u8(),
            ammo_magazine: 30,
            ammo_reserve: 120,
            last_input_tick: 10,
            last_shot_tick: None,
            last_command_tick: None,
            shot_index: 0,
        };
        let checkpoint = M3MatchCheckpointV2 {
            wire_version: M3_WIRE_VERSION,
            combat: MatchCheckpoint {
                source_epoch: 1,
                tick: 10,
                riders: players.into_iter().map(rider).collect(),
                resolved_shots: Vec::new(),
            },
            gameplay: authority.actors().checkpoint(),
            reloads: authority.reload_checkpoints(),
            next_horse_damage_sequence: authority.next_horse_damage_sequence(),
            bounty: {
                let mut bounty =
                    BountyMatchKernel::new(1, 0, SimulationTick::new(0), players.to_vec()).unwrap();
                bounty.advance_tick(tick).unwrap();
                bounty.checkpoint()
            },
        };
        let (restored, restored_bounty) =
            PeerSession::restore_m3_checkpoint(&checkpoint, 2).unwrap();
        assert_eq!(restored.actors().authority_epoch(), 2);
        assert_eq!(restored_bounty.authority_epoch(), 2);
        for player in players {
            assert_eq!(
                restored.actors().horse_entity_id(player),
                Some(horse_entity_id(player))
            );
        }

        let mut wrong_bank = M3AuthorityBank::new(1);
        for (index, player) in players.into_iter().enumerate() {
            assert!(wrong_bank.register_actor(
                player,
                EntityId(900 + u64::try_from(index).unwrap()),
                HorseVitalityClass::Courser,
            ));
            wrong_bank
                .advance_actor(
                    player,
                    ActorM3TickInput {
                        tick,
                        on_foot: OnFootTickInput {
                            tick,
                            move_direction: None,
                            sprint_pressed: false,
                            crouch_pressed: false,
                            reload_active: false,
                        },
                        interact_pressed: false,
                        spur_pressed: false,
                        mounted_for_spur: true,
                        rider_position: QuantizedOrigin::default(),
                        return_horse_position: QuantizedOrigin::default(),
                        return_horse_moving: false,
                    },
                )
                .unwrap();
        }
        let forged = M3MatchCheckpointV2 {
            gameplay: wrong_bank.checkpoint(),
            ..checkpoint
        };
        assert!(forged.is_bounded_and_canonical());
        assert!(PeerSession::restore_m3_checkpoint(&forged, 2).is_none());
    }

    #[test]
    fn parse_destination_accepts_ipv4_endpoint() {
        let parsed = parse_destination("100.64.1.2", 41643).expect("valid IPv4 endpoint");
        assert_eq!(parsed, SocketAddr::from(([100, 64, 1, 2], 41643)));
        assert!(parsed.is_ipv4());
    }

    #[test]
    fn parse_destination_accepts_ipv6_endpoint() {
        // Regression: `format!("{ip}:{port}")` left every bare IPv6 literal
        // unparseable, so an IPv6-selected peer registered but could not send.
        let parsed = parse_destination("fd7a:115c:a1e0::1", 41643).expect("valid IPv6 endpoint");
        assert_eq!(parsed, "[fd7a:115c:a1e0::1]:41643".parse().unwrap());
        assert!(parsed.is_ipv6());
    }

    #[test]
    fn parse_destination_rejects_invalid_endpoints() {
        assert_eq!(
            parse_destination("::1", 0),
            Some("[::1]:0".parse().unwrap())
        );
        assert!(parse_destination("100.64.1.2", 65_535).is_some());
        assert!(parse_destination("100.64.1.2", 65_536).is_none());
        assert!(parse_destination("::1", -1).is_none());
        assert!(parse_destination("[fd7a::1]", 41643).is_none());
        assert!(parse_destination("100.64.1.2:41643", 41643).is_none());
        assert!(parse_destination("not-an-ip", 41643).is_none());
        assert!(parse_destination("", 41643).is_none());
    }

    #[tokio::test]
    async fn enrollment_stop_cancels_in_flight_connect() {
        // Leave/quit while enrollment still owns the one-use credential.
        let (sender, mut commands) = tokio_mpsc::unbounded_channel();
        sender.send(WorkerCommand::Stop).unwrap();
        let mut deferred = Vec::new();
        let outcome = enroll_with_cancellation(
            std::future::pending::<Result<(), std::io::Error>>(),
            &mut commands,
            &mut deferred,
        )
        .await;
        assert!(matches!(outcome, EnrollOutcome::Cancelled));
        assert!(deferred.is_empty());
    }

    #[tokio::test]
    async fn enrollment_disconnect_cancels_in_flight_connect() {
        // The Godot-side sender going away (node freed) must not trap the
        // credential inside a pending connect either.
        let (sender, mut commands) = tokio_mpsc::unbounded_channel::<WorkerCommand>();
        drop(sender);
        let mut deferred = Vec::new();
        let outcome = enroll_with_cancellation(
            std::future::pending::<Result<(), std::io::Error>>(),
            &mut commands,
            &mut deferred,
        )
        .await;
        assert!(matches!(outcome, EnrollOutcome::Cancelled));
    }

    #[tokio::test]
    async fn enrollment_stop_wins_over_completed_connect() {
        // Even when the connect future has already resolved, a Stop queued
        // during the enrollment frame must cancel the session.
        let (sender, mut commands) = tokio_mpsc::unbounded_channel();
        sender.send(WorkerCommand::Stop).unwrap();
        let mut deferred = Vec::new();
        let outcome = enroll_with_cancellation(
            async { Ok::<_, std::io::Error>(7_u8) },
            &mut commands,
            &mut deferred,
        )
        .await;
        assert!(matches!(outcome, EnrollOutcome::Cancelled));
    }

    #[tokio::test]
    async fn enrollment_defers_queued_gameplay_commands_in_order() {
        let (sender, mut commands) = tokio_mpsc::unbounded_channel();
        sender
            .send(WorkerCommand::QueryRoute {
                peer_ip: IpAddr::from([100, 64, 0, 1]),
            })
            .unwrap();
        sender
            .send(WorkerCommand::Send {
                packet: vec![1, 2, 3],
                destination: SocketAddr::from(([100, 64, 0, 2], 41643)),
            })
            .unwrap();
        let mut deferred = Vec::new();
        let outcome = enroll_with_cancellation(
            async { Ok::<_, std::io::Error>(42_u8) },
            &mut commands,
            &mut deferred,
        )
        .await;
        assert!(matches!(outcome, EnrollOutcome::Connected(42)));
        // Both gameplay commands survived enrollment in original order.
        assert_eq!(deferred.len(), 2);
        assert!(matches!(deferred[0], WorkerCommand::QueryRoute { .. }));
        assert!(matches!(deferred[1], WorkerCommand::Send { .. }));
    }

    #[tokio::test]
    async fn enrollment_reports_connect_failure() {
        let (_sender, mut commands) = tokio_mpsc::unbounded_channel::<WorkerCommand>();
        let mut deferred = Vec::new();
        let outcome = enroll_with_cancellation(
            async { Err::<u8, _>(std::io::Error::other("enrollment rejected")) },
            &mut commands,
            &mut deferred,
        )
        .await;
        match outcome {
            EnrollOutcome::Failed(message) => assert!(message.contains("enrollment rejected")),
            _ => panic!("expected failed enrollment"),
        }
    }
}
