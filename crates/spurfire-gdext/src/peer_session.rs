//! Godot-facing background RustScale application-UDP node.

use std::{
    net::{IpAddr, SocketAddr},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
};

use godot::classes::{INode, Node};
use godot::prelude::*;
use spurfire_net::{
    decode, encode, rustscale::RustScalePeer, AcceptOutcome, PeerPayload, SessionState,
};
use spurfire_protocol::{LobbyId, PlayerId, RiderStance};
use tokio::time::Duration;
use zeroize::Zeroizing;

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
    Connected { ip: String, port: u16 },
    Packet { packet: Vec<u8>, source: SocketAddr },
    Route { peer_ip: String, route: String },
    Failed(String),
    Stopped,
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

fn run_worker(
    hostname: String,
    auth_key: Zeroizing<String>,
    port: u16,
    commands: Receiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = events.send(WorkerEvent::Failed(format!(
                "network runtime failed: {error}"
            )));
            return;
        }
    };
    runtime.block_on(async move {
        let mut peer = match RustScalePeer::connect(hostname, auth_key, port).await {
            Ok(peer) => peer,
            Err(error) => {
                let _ = events.send(WorkerEvent::Failed(error.to_string()));
                return;
            }
        };
        let _ = events.send(WorkerEvent::Connected {
            ip: peer.tailnet_ip().to_string(),
            port: peer.local_addr().port(),
        });
        'network: loop {
            // Drain every queued gameplay frame before waiting for receive. The
            // old one-command-per-25ms loop could create avoidable input latency
            // whenever physics frames arrived in a burst.
            loop {
                match commands.try_recv() {
                    Ok(WorkerCommand::Send {
                        packet,
                        destination,
                    }) => match decode(&packet) {
                        Ok(envelope) => {
                            if let Err(error) = peer.send(&envelope, destination).await {
                                let _ = events.send(WorkerEvent::Failed(error.to_string()));
                            }
                        }
                        Err(error) => {
                            let _ = events.send(WorkerEvent::Failed(error.to_string()));
                        }
                    },
                    Ok(WorkerCommand::QueryRoute { peer_ip }) => {
                        let route = peer.route_to(peer_ip).unwrap_or_else(|| "Unknown".into());
                        let _ = events.send(WorkerEvent::Route {
                            peer_ip: peer_ip.to_string(),
                            route,
                        });
                    }
                    Ok(WorkerCommand::Stop) | Err(TryRecvError::Disconnected) => break 'network,
                    Err(TryRecvError::Empty) => break,
                }
            }
            match peer.recv(Duration::from_millis(5)).await {
                Ok((envelope, source)) => match encode(&envelope) {
                    Ok(packet) => {
                        let _ = events.send(WorkerEvent::Packet { packet, source });
                    }
                    Err(error) => {
                        let _ = events.send(WorkerEvent::Failed(error.to_string()));
                    }
                },
                Err(spurfire_net::rustscale::RustScaleTransportError::Timeout) => {}
                Err(error) => {
                    let _ = events.send(WorkerEvent::Failed(error.to_string()));
                    break;
                }
            }
        }
        if let Err(error) = close_peer(&mut peer).await {
            let _ = events.send(WorkerEvent::Failed(error));
        }
        let _ = events.send(WorkerEvent::Stopped);
    });
}

/// A non-blocking Godot `Node` that owns an embedded ephemeral RustScale peer.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct PeerSession {
    #[base]
    base: Base<Node>,
    #[var(no_set)]
    connection_state: GString,
    #[var(no_set)]
    tailnet_ip: GString,
    #[var(no_set)]
    local_port: i64,
    #[var(no_set)]
    authority_player_id: GString,
    #[var(no_set)]
    authority_epoch: i64,
    session: Option<SessionState>,
    command_tx: Option<Sender<WorkerCommand>>,
    event_rx: Option<Receiver<WorkerEvent>>,
}

#[godot_api]
impl PeerSession {
    #[signal]
    fn connected(tailnet_ip: GString, local_port: i64);
    #[signal]
    fn packet_received(packet: PackedByteArray, source_ip: GString, source_port: i64);
    #[signal]
    fn route_updated(peer_ip: GString, route: GString);
    #[signal]
    fn connection_failed(message: GString);
    #[signal]
    fn disconnected();

    /// Configure validated application identity and deterministic authority state.
    #[func]
    fn configure_session(
        &mut self,
        lobby_id: GString,
        local_player_id: GString,
        authority_player_id: GString,
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
        self.session = Some(SessionState::new(lobby_id, local_player, authority, now_ms));
        self.authority_player_id = GString::from(&authority.to_string());
        self.authority_epoch = 1;
        true
    }

    /// Build a bounded, sequenced heartbeat datagram for `send_packet`.
    #[func]
    fn make_heartbeat(&mut self, tick: i64) -> PackedByteArray {
        self.make_packet(tick, PeerPayload::Heartbeat)
    }

    /// Build an application-channel RTT probe or response.
    #[func]
    fn make_probe(&mut self, tick: i64, nonce: i64, reply: bool) -> PackedByteArray {
        let Ok(nonce) = u64::try_from(nonce) else {
            return PackedByteArray::new();
        };
        self.make_packet(tick, PeerPayload::Probe { nonce, reply })
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
        position: Vector3,
        velocity: Vector3,
        yaw_degrees: f64,
        stance_id: i64,
    ) -> PackedByteArray {
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
            || !(RiderStance::MOUNTED_ID..=RiderStance::ON_FOOT_STANDING_ID)
                .contains(&stance_id)
        {
            return PackedByteArray::new();
        }
        self.make_packet(
            tick,
            PeerPayload::RiderSnapshot {
                position_mm: [position_mm[0], position_mm[1], position_mm[2]],
                velocity_mmps: [velocity_mmps[0], velocity_mmps[1], velocity_mmps[2]],
                yaw_millidegrees: yaw.round() as i32,
                stance: RiderStance::from_u8(stance_id),
            },
        )
    }

    /// Decode presentation fields after `accept_packet` has accepted the packet.
    #[func]
    fn decode_packet(&self, packet: PackedByteArray) -> VarDictionary {
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
                position_mm,
                velocity_mmps,
                yaw_millidegrees,
                stance,
            } => {
                result.set("type", "rider_snapshot");
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
            PeerPayload::ShotCommand { .. } => result.set("type", "shot_command"),
            PeerPayload::ShotResult { .. } => result.set("type", "shot_result"),
            PeerPayload::Authority { authority, epoch } => {
                result.set("type", "authority");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
            }
            PeerPayload::MigrationSnapshot {
                authority,
                epoch,
                tick,
                state_hash,
            } => {
                result.set("type", "migration_snapshot");
                result.set("authority", authority.to_string());
                result.set("epoch", i64::try_from(epoch).unwrap_or(i64::MAX));
                result.set("snapshot_tick", i64::try_from(tick).unwrap_or(i64::MAX));
                result.set("state_hash", state_hash);
            }
            PeerPayload::Leave => result.set("type", "leave"),
        }
        result
    }

    /// Validate one received packet. 0=accepted, 1=replay, 2=wrong lobby, 3=stale epoch, -1=invalid.
    #[func]
    fn accept_packet(&mut self, packet: PackedByteArray, now_ms: i64) -> i64 {
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
        match outcome {
            AcceptOutcome::Accepted => 0,
            AcceptOutcome::DuplicateOrReplay => 1,
            AcceptOutcome::WrongLobby => 2,
            AcceptOutcome::StaleAuthorityEpoch => 3,
        }
    }

    /// Start enrollment on a background Tokio runtime. The auth key is never logged.
    #[func]
    fn connect_rustscale(&mut self, hostname: GString, auth_key: GString, port: i64) -> bool {
        if self.command_tx.is_some() || !(0..=u16::MAX as i64).contains(&port) {
            return false;
        }
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let hostname = hostname.to_string();
        let auth_key = Zeroizing::new(auth_key.to_string());
        if let Err(error) = thread::Builder::new()
            .name("spurfire-rustscale".into())
            .spawn(move || run_worker(hostname, auth_key, port as u16, command_rx, event_tx))
        {
            godot_error!("PeerSession could not start worker: {error}");
            return false;
        }
        self.command_tx = Some(command_tx);
        self.event_rx = Some(event_rx);
        self.connection_state = "connecting".into();
        true
    }

    /// Send one bounded Spurfire envelope returned by the protocol codec.
    #[func]
    fn send_packet(&mut self, packet: PackedByteArray, destination_ip: GString, port: i64) -> bool {
        let Ok(destination) = format!("{}:{port}", destination_ip).parse::<SocketAddr>() else {
            return false;
        };
        let Some(sender) = &self.command_tx else {
            return false;
        };
        sender
            .send(WorkerCommand::Send {
                packet: packet.to_vec(),
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

    #[func]
    fn shutdown(&mut self) {
        if let Some(sender) = self.command_tx.take() {
            let _ = sender.send(WorkerCommand::Stop);
        }
        self.connection_state = "disconnecting".into();
    }

    fn make_packet(&mut self, tick: i64, payload: PeerPayload) -> PackedByteArray {
        let (Some(session), Ok(tick)) = (self.session.as_mut(), u64::try_from(tick)) else {
            return PackedByteArray::new();
        };
        encode(&session.envelope(tick, payload)).map_or_else(
            |_| PackedByteArray::new(),
            |bytes| PackedByteArray::from(bytes.as_slice()),
        )
    }

    fn poll_events(&mut self) {
        let Some(receiver) = self.event_rx.take() else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(WorkerEvent::Connected { ip, port }) => {
                    self.connection_state = "connected".into();
                    self.tailnet_ip = GString::from(&ip);
                    self.local_port = i64::from(port);
                    let signal_ip = GString::from(&ip);
                    self.signals().connected().emit(&signal_ip, i64::from(port));
                }
                Ok(WorkerEvent::Packet { packet, source }) => {
                    let signal_packet = PackedByteArray::from(packet.as_slice());
                    let source_ip = source.ip().to_string();
                    let signal_ip = GString::from(&source_ip);
                    self.signals().packet_received().emit(
                        &signal_packet,
                        &signal_ip,
                        i64::from(source.port()),
                    );
                }
                Ok(WorkerEvent::Route { peer_ip, route }) => {
                    let signal_ip = GString::from(&peer_ip);
                    let signal_route = GString::from(&route);
                    self.signals()
                        .route_updated()
                        .emit(&signal_ip, &signal_route);
                }
                Ok(WorkerEvent::Failed(message)) => {
                    self.connection_state = "error".into();
                    let signal_message = GString::from(&message);
                    self.signals().connection_failed().emit(&signal_message);
                }
                Ok(WorkerEvent::Stopped) => {
                    self.connection_state = "offline".into();
                    self.command_tx = None;
                    self.signals().disconnected().emit();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.command_tx = None;
                    break;
                }
            }
        }
        self.event_rx = Some(receiver);
    }
}

#[godot_api]
impl INode for PeerSession {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            connection_state: "offline".into(),
            tailnet_ip: GString::new(),
            local_port: 0,
            authority_player_id: GString::new(),
            authority_epoch: 0,
            session: None,
            command_tx: None,
            event_rx: None,
        }
    }

    fn process(&mut self, _delta: f64) {
        self.poll_events();
    }
    fn exit_tree(&mut self) {
        self.shutdown();
    }
}
