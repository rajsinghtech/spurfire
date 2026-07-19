use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt,
    io::{self, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket},
    process::{Child, Command, Stdio},
    str::FromStr,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use spurfire_net::{
    decode, encode, AcceptOutcome, Envelope, MatchCheckpoint, PeerPayload, RiderCheckpoint,
    SecureSession, SessionState, MAX_DATAGRAM_BYTES, RIDER_INPUT_JUMP_PRESSED,
};
use spurfire_protocol::{
    canonical_manifest_digest, LobbyId, PlayerId, QuantizedDirection, QuantizedOrigin, RiderStance,
    RosterManifest, RosterManifestEntry, SessionPublicKey, SessionSignature, ShotCommand,
    ShotOutcome, ShotResult, SimulationTick, WeaponId,
};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);
const PROOF_TIMEOUT: Duration = Duration::from_secs(20);
const UDP_TIMEOUT: Duration = Duration::from_millis(25);
const SEND_INTERVAL: Duration = Duration::from_millis(75);
const CONTROL_HEADER_BYTES: usize = size_of::<u32>();
const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024;

type ProofResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
type ReaderHandles = BTreeMap<Node, JoinHandle<ProofResult<()>>>;
type ActiveControl = (BTreeMap<Node, TcpStream>, Receiver<Notice>, ReaderHandles);
type StartedScenario = (
    Vec<PeerProcess>,
    BTreeMap<Node, TcpStream>,
    Receiver<Notice>,
    ReaderHandles,
);

fn proof_error(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::other(message.into()).into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    TwoPeer,
    Migration,
}

impl Scenario {
    const fn nodes(self) -> &'static [Node] {
        match self {
            Self::TwoPeer => &[Node::A, Node::B],
            Self::Migration => &[Node::A, Node::B, Node::C],
        }
    }

    const fn lobby(self) -> &'static str {
        match self {
            Self::TwoPeer => "00000000-0000-4000-8000-000000000021",
            Self::Migration => "00000000-0000-4000-8000-000000000031",
        }
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TwoPeer => "two_peer",
            Self::Migration => "migration",
        })
    }
}

impl FromStr for Scenario {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "two_peer" => Ok(Self::TwoPeer),
            "migration" => Ok(Self::Migration),
            _ => Err(format!("unknown proof scenario: {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Node {
    A,
    B,
    C,
}

impl Node {
    const fn player(self) -> &'static str {
        match self {
            Self::A => "00000000-0000-4000-8000-000000000001",
            Self::B => "00000000-0000-4000-8000-000000000002",
            Self::C => "00000000-0000-4000-8000-000000000003",
        }
    }

    const fn key_seed(self) -> u8 {
        match self {
            Self::A => 1,
            Self::B => 2,
            Self::C => 3,
        }
    }

    const fn identity_ip(self) -> Ipv4Addr {
        match self {
            Self::A => Ipv4Addr::new(100, 64, 0, 11),
            Self::B => Ipv4Addr::new(100, 64, 0, 12),
            Self::C => Ipv4Addr::new(100, 64, 0, 13),
        }
    }

    fn id(self) -> ProofResult<PlayerId> {
        Ok(PlayerId::parse(self.player())?)
    }

    fn signing_key(self) -> SigningKey {
        SigningKey::from_bytes(&[self.key_seed(); 32])
    }
}

impl fmt::Display for Node {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
        })
    }
}

impl FromStr for Node {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            "c" => Ok(Self::C),
            _ => Err(format!("unknown proof node: {value}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventKind {
    Mesh,
    HelloSignatureRejected,
    SignedHelloAccepted,
    InputSignatureRejected,
    SignedInputAccepted,
    ShotCommandAccepted,
    ShotResultAccepted,
    ShotResultDuplicateRejected,
    Migrated,
    MigrationClaimSignatureRejected,
    SignedMigrationClaimAccepted,
    GameplaySignatureRejected,
    NonAuthoritySnapshotRejected,
    SignedGameplayAccepted,
    SignedAuthoritySnapshotAccepted,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ControlMessage {
    Ready {
        scenario: Scenario,
        node: Node,
        endpoint: SocketAddr,
    },
    Config {
        scenario: Scenario,
        manifest: RosterManifest,
        manifest_public_key: SessionPublicKey,
        manifest_signature: SessionSignature,
    },
    Event {
        node: Node,
        kind: EventKind,
        authority: Node,
        epoch: u64,
    },
    Quiesce,
    Quiesced {
        node: Node,
    },
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EventKey {
    node: Node,
    kind: EventKind,
}

#[derive(Clone, Copy, Debug)]
struct EventRecord {
    node: Node,
    kind: EventKind,
    authority: Node,
    epoch: u64,
}

enum Notice {
    Event(EventRecord),
    Quiesced(Node),
    Closed(Node),
    Failed(Node, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentCommand {
    Quiesce,
    Stop,
}

struct ReadyConnection {
    node: Node,
    endpoint: SocketAddr,
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

struct PeerProcess {
    node: Node,
    child: Option<Child>,
}

fn encode_control_frame(message: &ControlMessage) -> ProofResult<Vec<u8>> {
    let payload = serde_json::to_vec(message)?;
    if payload.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(proof_error("control frame exceeds the application limit"));
    }
    let length = u32::try_from(payload.len())?;
    let mut frame = Vec::with_capacity(CONTROL_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn send_control<W: Write>(writer: &mut W, message: &ControlMessage) -> ProofResult<()> {
    let frame = encode_control_frame(message)?;
    // Build the complete application frame before the sole write_all call. A
    // successful return therefore means no lifecycle transition can bisect it.
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

fn read_control<R: Read>(reader: &mut R) -> ProofResult<Option<ControlMessage>> {
    let mut header = [0_u8; CONTROL_HEADER_BYTES];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    reader
        .read_exact(&mut header[1..])
        .map_err(|error| proof_error(format!("truncated control frame header: {error}")))?;
    let length = usize::try_from(u32::from_be_bytes(header))?;
    if length > MAX_CONTROL_FRAME_BYTES {
        return Err(proof_error(format!(
            "control frame length {length} exceeds limit {MAX_CONTROL_FRAME_BYTES}"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| proof_error(format!("truncated control frame body: {error}")))?;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| proof_error(format!("invalid complete control frame: {error}")))
}

fn spawn_peers(scenario: Scenario, control: SocketAddr) -> ProofResult<Vec<PeerProcess>> {
    let executable = env::current_exe()?;
    let mut peers = Vec::new();
    for node in scenario.nodes() {
        let child = Command::new(&executable)
            .arg("--child")
            .arg(scenario.to_string())
            .arg(node.to_string())
            .arg(control.to_string())
            .env_clear()
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        peers.push(PeerProcess {
            node: *node,
            child: Some(child),
        });
    }
    Ok(peers)
}

fn terminate_peers(peers: &mut [PeerProcess]) {
    for peer in peers {
        if let Some(mut child) = peer.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

fn accept_ready(
    listener: TcpListener,
    scenario: Scenario,
) -> ProofResult<BTreeMap<Node, ReadyConnection>> {
    let count = scenario.nodes().len();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = (|| -> ProofResult<BTreeMap<Node, ReadyConnection>> {
            let mut connections = BTreeMap::new();
            for _ in 0..count {
                let (stream, _) = listener.accept()?;
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(CONTROL_TIMEOUT))?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let message = read_control(&mut reader)?
                    .ok_or_else(|| proof_error("peer closed before its ready message"))?;
                let ControlMessage::Ready {
                    scenario: ready_scenario,
                    node,
                    endpoint,
                } = message
                else {
                    return Err(proof_error("peer sent a non-ready first message"));
                };
                if ready_scenario != scenario || !scenario.nodes().contains(&node) {
                    return Err(proof_error("peer ready message did not match the scenario"));
                }
                if connections
                    .insert(
                        node,
                        ReadyConnection {
                            node,
                            endpoint,
                            writer: stream,
                            reader,
                        },
                    )
                    .is_some()
                {
                    return Err(proof_error("duplicate peer ready message"));
                }
            }
            Ok(connections)
        })();
        let _ = sender.send(result);
    });

    receiver
        .recv_timeout(CONTROL_TIMEOUT)
        .map_err(|_| proof_error("timed out accepting proof peer processes"))?
}

fn config_for(
    scenario: Scenario,
    connections: &BTreeMap<Node, ReadyConnection>,
) -> ProofResult<ControlMessage> {
    let lobby = LobbyId::parse(scenario.lobby())?;
    let entries = scenario
        .nodes()
        .iter()
        .map(|node| {
            let connection = connections
                .get(node)
                .ok_or_else(|| proof_error(format!("missing ready peer {node}")))?;
            Ok(RosterManifestEntry {
                player_id: node.id()?,
                session_public_key: SessionPublicKey::from_bytes(
                    node.signing_key().verifying_key().to_bytes(),
                ),
                // Signed identities stay unique while proof transport uses the
                // only loopback address bindable on stock BSD/macOS.
                tailnet_address: node.identity_ip().into(),
                application_port: connection.endpoint.port(),
                node_key: None,
            })
        })
        .collect::<ProofResult<Vec<_>>>()?;
    let manifest = RosterManifest {
        lobby_id: lobby,
        network_generation: 7,
        session_generation: 11,
        roster_revision: 13,
        entries,
    };
    let manifest_signing = SigningKey::from_bytes(&[99; 32]);
    let manifest_public_key =
        SessionPublicKey::from_bytes(manifest_signing.verifying_key().to_bytes());
    let manifest_signature = SessionSignature::from_bytes(
        manifest_signing
            .sign(&canonical_manifest_digest(manifest_public_key, &manifest))
            .to_bytes(),
    );
    Ok(ControlMessage::Config {
        scenario,
        manifest,
        manifest_public_key,
        manifest_signature,
    })
}

fn notice_from_control(
    expected_node: Node,
    message: ControlMessage,
    terminal_frame_seen: &mut bool,
) -> ProofResult<Notice> {
    if *terminal_frame_seen {
        return Err(proof_error(
            "peer sent a frame after its lifecycle acknowledgment",
        ));
    }
    match message {
        ControlMessage::Event {
            node,
            kind,
            authority,
            epoch,
        } if node == expected_node => {
            *terminal_frame_seen = kind == EventKind::Done;
            Ok(Notice::Event(EventRecord {
                node,
                kind,
                authority,
                epoch,
            }))
        }
        ControlMessage::Quiesced { node } if node == expected_node => {
            *terminal_frame_seen = true;
            Ok(Notice::Quiesced(node))
        }
        _ => Err(proof_error("peer sent an invalid control event")),
    }
}

fn start_event_readers(
    mut connections: BTreeMap<Node, ReadyConnection>,
    config: &ControlMessage,
) -> ProofResult<ActiveControl> {
    let (sender, receiver) = mpsc::channel();
    let mut writers = BTreeMap::new();
    let mut readers = BTreeMap::new();
    for (node, connection) in &connections {
        if *node != connection.node {
            return Err(proof_error("ready connection node mismatch"));
        }
    }
    // Complete every fallible setup operation before spawning a reader. This
    // ensures an initialization failure cannot leave a detached reader behind.
    for connection in connections.values_mut() {
        send_control(&mut connection.writer, config)?;
        connection.writer.set_read_timeout(None)?;
    }
    for (node, connection) in std::mem::take(&mut connections) {
        let mut reader = connection.reader;
        let event_sender = sender.clone();
        let handle = thread::spawn(move || {
            let mut terminal_frame_seen = false;
            loop {
                let message = match read_control(&mut reader) {
                    Ok(Some(message)) => message,
                    Ok(None) => {
                        let _ = event_sender.send(Notice::Closed(node));
                        return Ok(());
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = event_sender.send(Notice::Failed(node, message.clone()));
                        return Err(proof_error(message));
                    }
                };
                match notice_from_control(node, message, &mut terminal_frame_seen) {
                    Ok(notice) => {
                        if event_sender.send(notice).is_err() {
                            return Err(proof_error("proof event receiver disconnected"));
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = event_sender.send(Notice::Failed(node, message.clone()));
                        return Err(proof_error(message));
                    }
                }
            }
        });
        readers.insert(node, handle);
        writers.insert(node, connection.writer);
    }
    drop(sender);
    Ok((writers, receiver, readers))
}

fn close_writers(writers: &mut BTreeMap<Node, TcpStream>) {
    for writer in writers.values() {
        let _ = writer.shutdown(Shutdown::Write);
    }
    writers.clear();
}

fn join_reader(
    readers: &mut BTreeMap<Node, JoinHandle<ProofResult<()>>>,
    node: Node,
) -> ProofResult<()> {
    let handle = readers
        .remove(&node)
        .ok_or_else(|| proof_error(format!("missing control reader for peer {node}")))?;
    handle
        .join()
        .map_err(|_| proof_error(format!("peer {node} control reader panicked")))?
}

fn join_readers(readers: &mut BTreeMap<Node, JoinHandle<ProofResult<()>>>) -> ProofResult<()> {
    let mut first_error = None;
    for node in [Node::A, Node::B, Node::C] {
        if readers.contains_key(&node) {
            if let Err(error) = join_reader(readers, node) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn validate_event(scenario: Scenario, event: EventRecord) -> ProofResult<EventKey> {
    let (expected_authority, expected_epoch) = match scenario {
        Scenario::TwoPeer => (Node::A, 1),
        Scenario::Migration if event.kind == EventKind::Mesh => (Node::A, 1),
        Scenario::Migration => (Node::B, 2),
    };
    if event.authority != expected_authority || event.epoch != expected_epoch {
        return Err(proof_error(format!(
            "peer {} reported {:?} at authority {} epoch {}, expected {} epoch {}",
            event.node,
            event.kind,
            event.authority,
            event.epoch,
            expected_authority,
            expected_epoch
        )));
    }
    Ok(EventKey {
        node: event.node,
        kind: event.kind,
    })
}

fn collect_until(
    scenario: Scenario,
    receiver: &Receiver<Notice>,
    required: &BTreeSet<EventKey>,
    seen: &mut BTreeSet<EventKey>,
    timeout: Duration,
) -> ProofResult<()> {
    let deadline = Instant::now() + timeout;
    while !required.is_subset(seen) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(proof_error(format!(
                "timed out waiting for proof events: {:?}",
                required.difference(seen).collect::<Vec<_>>()
            )));
        }
        match receiver.recv_timeout(remaining) {
            Ok(Notice::Event(event)) => {
                seen.insert(validate_event(scenario, event)?);
            }
            Ok(Notice::Quiesced(node)) => {
                return Err(proof_error(format!(
                    "peer {node} quiesced while proof events were still required"
                )));
            }
            Ok(Notice::Closed(node)) => {
                if required.difference(seen).any(|event| event.node == node) {
                    return Err(proof_error(format!(
                        "peer {node} closed before its required proof events"
                    )));
                }
            }
            Ok(Notice::Failed(node, message)) => {
                return Err(proof_error(format!(
                    "peer {node} control failure: {message}"
                )));
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(proof_error("timed out waiting for proof events"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(proof_error("all proof event channels disconnected"));
            }
        }
    }
    Ok(())
}

fn wait_for_quiesced(
    scenario: Scenario,
    receiver: &Receiver<Notice>,
    node: Node,
    seen: &mut BTreeSet<EventKey>,
    timeout: Duration,
) -> ProofResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(proof_error(format!(
                "timed out waiting for peer {node} quiesced"
            )));
        }
        match receiver.recv_timeout(remaining) {
            Ok(Notice::Event(event)) => {
                seen.insert(validate_event(scenario, event)?);
            }
            Ok(Notice::Quiesced(quiesced)) if quiesced == node => return Ok(()),
            Ok(Notice::Quiesced(quiesced)) => {
                return Err(proof_error(format!("unexpected peer {quiesced} quiesced")));
            }
            Ok(Notice::Closed(closed)) => {
                return Err(proof_error(format!(
                    "peer {closed} closed before peer {node} quiesced"
                )));
            }
            Ok(Notice::Failed(failed, message)) => {
                return Err(proof_error(format!(
                    "peer {failed} control failure: {message}"
                )));
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(proof_error(format!(
                    "timed out waiting for peer {node} quiesced"
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(proof_error("all proof event channels disconnected"));
            }
        }
    }
}

fn wait_for_success(peers: &mut [PeerProcess], expected_killed: Option<Node>) -> ProofResult<()> {
    for peer in peers {
        let Some(mut child) = peer.child.take() else {
            continue;
        };
        let status = child.wait()?;
        if Some(peer.node) == expected_killed {
            if status.success() {
                return Err(proof_error("authority process was expected to be killed"));
            }
        } else if !status.success() {
            return Err(proof_error(format!(
                "peer {} exited with {status}",
                peer.node
            )));
        }
    }
    Ok(())
}

fn start_scenario(scenario: Scenario) -> ProofResult<StartedScenario> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let control = listener.local_addr()?;
    let mut peers = spawn_peers(scenario, control)?;
    let result = (|| {
        let connections = accept_ready(listener, scenario)?;
        let config = config_for(scenario, &connections)?;
        start_event_readers(connections, &config)
    })();
    match result {
        Ok((writers, receiver, readers)) => Ok((peers, writers, receiver, readers)),
        Err(error) => {
            terminate_peers(&mut peers);
            Err(error)
        }
    }
}

fn run_two_peer_proof() -> ProofResult<()> {
    let (mut peers, mut writers, receiver, mut readers) = start_scenario(Scenario::TwoPeer)?;
    let result = (|| {
        let required = BTreeSet::from([
            EventKey {
                node: Node::B,
                kind: EventKind::HelloSignatureRejected,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::SignedHelloAccepted,
            },
            EventKey {
                node: Node::A,
                kind: EventKind::InputSignatureRejected,
            },
            EventKey {
                node: Node::A,
                kind: EventKind::SignedInputAccepted,
            },
            EventKey {
                node: Node::A,
                kind: EventKind::ShotCommandAccepted,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::ShotResultAccepted,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::ShotResultDuplicateRejected,
            },
            EventKey {
                node: Node::A,
                kind: EventKind::Done,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::Done,
            },
        ]);
        collect_until(
            Scenario::TwoPeer,
            &receiver,
            &required,
            &mut BTreeSet::new(),
            PROOF_TIMEOUT,
        )?;
        wait_for_success(&mut peers, None)?;
        close_writers(&mut writers);
        join_readers(&mut readers)?;
        println!(
            "SPURFIRE_SIGNED_TWO_PROCESS_OK peer_processes=2 signatures=strict accepted_bidirectional=true combat=authority_once result_dedup=true authority=a epoch=1"
        );
        Ok(())
    })();
    if result.is_err() {
        close_writers(&mut writers);
        terminate_peers(&mut peers);
        let _ = join_readers(&mut readers);
    }
    result
}

fn kill_peer(peers: &mut [PeerProcess], node: Node) -> ProofResult<()> {
    let peer = peers
        .iter_mut()
        .find(|peer| peer.node == node)
        .ok_or_else(|| proof_error(format!("missing peer process {node}")))?;
    let child = peer
        .child
        .as_mut()
        .ok_or_else(|| proof_error(format!("peer process {node} was already consumed")))?;
    child.kill()?;
    let status = child.wait()?;
    if status.success() {
        return Err(proof_error("authority process kill unexpectedly succeeded"));
    }
    peer.child = None;
    Ok(())
}

fn run_migration_proof() -> ProofResult<()> {
    let (mut peers, mut writers, receiver, mut readers) = start_scenario(Scenario::Migration)?;
    let result = (|| {
        let mesh = BTreeSet::from([
            EventKey {
                node: Node::B,
                kind: EventKind::Mesh,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::Mesh,
            },
        ]);
        let mut seen = BTreeSet::new();
        collect_until(
            Scenario::Migration,
            &receiver,
            &mesh,
            &mut seen,
            CONTROL_TIMEOUT,
        )?;
        let authority_writer = writers
            .get_mut(&Node::A)
            .ok_or_else(|| proof_error("missing control writer for authority peer a"))?;
        send_control(authority_writer, &ControlMessage::Quiesce)?;
        wait_for_quiesced(
            Scenario::Migration,
            &receiver,
            Node::A,
            &mut seen,
            CONTROL_TIMEOUT,
        )?;
        kill_peer(&mut peers, Node::A)?;
        if let Some(writer) = writers.remove(&Node::A) {
            writer.shutdown(Shutdown::Write)?;
        }
        join_reader(&mut readers, Node::A)?;

        let migrated = BTreeSet::from([
            EventKey {
                node: Node::B,
                kind: EventKind::Migrated,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::Migrated,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::MigrationClaimSignatureRejected,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::SignedMigrationClaimAccepted,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::GameplaySignatureRejected,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::NonAuthoritySnapshotRejected,
            },
            EventKey {
                node: Node::B,
                kind: EventKind::SignedGameplayAccepted,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::SignedAuthoritySnapshotAccepted,
            },
        ]);
        collect_until(
            Scenario::Migration,
            &receiver,
            &migrated,
            &mut seen,
            PROOF_TIMEOUT,
        )?;
        for node in [Node::B, Node::C] {
            let writer = writers
                .get_mut(&node)
                .ok_or_else(|| proof_error(format!("missing control writer for peer {node}")))?;
            send_control(writer, &ControlMessage::Stop)?;
        }
        let done = BTreeSet::from([
            EventKey {
                node: Node::B,
                kind: EventKind::Done,
            },
            EventKey {
                node: Node::C,
                kind: EventKind::Done,
            },
        ]);
        collect_until(
            Scenario::Migration,
            &receiver,
            &done,
            &mut seen,
            CONTROL_TIMEOUT,
        )?;
        wait_for_success(&mut peers, None)?;
        close_writers(&mut writers);
        join_readers(&mut readers)?;
        println!(
            "SPURFIRE_SIGNED_THREE_PROCESS_MIGRATION_OK peer_processes=3 signatures=strict authority_roles=strict authority=a successor=b epoch=2 agreement=b,c checkpoint=hash_checked riders=2 combat_receipts=retained continued_play=true"
        );
        Ok(())
    })();
    if result.is_err() {
        close_writers(&mut writers);
        terminate_peers(&mut peers);
        let _ = join_readers(&mut readers);
    }
    result
}

fn child_handshake(
    scenario: Scenario,
    node: Node,
    control: SocketAddr,
) -> ProofResult<(
    UdpSocket,
    TcpStream,
    BufReader<TcpStream>,
    SecureSession,
    SigningKey,
    RosterManifest,
)> {
    if !scenario.nodes().contains(&node) {
        return Err(proof_error(
            "node does not belong to the requested scenario",
        ));
    }
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.set_read_timeout(Some(UDP_TIMEOUT))?;
    socket.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let endpoint = socket.local_addr()?;

    let mut writer = TcpStream::connect_timeout(&control, CONTROL_TIMEOUT)?;
    writer.set_nodelay(true)?;
    writer.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let mut reader = BufReader::new(writer.try_clone()?);
    send_control(
        &mut writer,
        &ControlMessage::Ready {
            scenario,
            node,
            endpoint,
        },
    )?;
    let config = read_control(&mut reader)?
        .ok_or_else(|| proof_error("parent closed before sending the signed roster"))?;
    let ControlMessage::Config {
        scenario: configured_scenario,
        manifest,
        manifest_public_key,
        manifest_signature,
    } = config
    else {
        return Err(proof_error("parent sent a non-config control message"));
    };
    if configured_scenario != scenario || manifest.lobby_id != LobbyId::parse(scenario.lobby())? {
        return Err(proof_error("signed roster scenario mismatch"));
    }
    let signing_key = node.signing_key();
    let local_player = node.id()?;
    let local_entry = manifest
        .entries
        .iter()
        .find(|entry| entry.player_id == local_player)
        .ok_or_else(|| proof_error("local player missing from signed roster"))?;
    if local_entry.tailnet_address != node.identity_ip()
        || local_entry.application_port != endpoint.port()
        || local_entry.session_public_key.as_bytes() != &signing_key.verifying_key().to_bytes()
    {
        return Err(proof_error(
            "local process did not match its signed roster entry",
        ));
    }

    let lobby = LobbyId::parse(scenario.lobby())?;
    let mut state = SessionState::new(lobby, local_player, Node::A.id()?, 0);
    for peer in scenario.nodes() {
        state.add_peer(peer.id()?, 0);
    }
    let session = SecureSession::new(
        manifest.clone(),
        manifest_public_key,
        manifest_signature,
        state,
    )?;
    Ok((socket, writer, reader, session, signing_key, manifest))
}

fn endpoint_for(manifest: &RosterManifest, node: Node) -> ProofResult<SocketAddr> {
    let player = node.id()?;
    let entry = manifest
        .entries
        .iter()
        .find(|entry| entry.player_id == player)
        .ok_or_else(|| proof_error(format!("signed roster missing peer {node}")))?;
    Ok(SocketAddr::new(
        Ipv4Addr::LOCALHOST.into(),
        entry.application_port,
    ))
}

fn send_envelope(
    socket: &UdpSocket,
    envelope: &Envelope,
    destination: SocketAddr,
) -> ProofResult<()> {
    let bytes = encode(envelope)?;
    let sent = socket.send_to(&bytes, destination)?;
    if sent != bytes.len() {
        return Err(proof_error("proof UDP send was truncated"));
    }
    Ok(())
}

fn receive_envelope(
    socket: &UdpSocket,
    manifest: Option<&RosterManifest>,
) -> ProofResult<(Envelope, SocketAddr)> {
    let mut bytes = [0_u8; MAX_DATAGRAM_BYTES];
    let (length, mut source) = socket.recv_from(&mut bytes)?;
    if let Some(entry) = manifest.and_then(|manifest| {
        manifest
            .entries
            .iter()
            .find(|entry| entry.application_port == source.port())
    }) {
        source.set_ip(entry.tailnet_address);
    }
    Ok((decode(&bytes[..length])?, source))
}

fn transient_receive_error(error: &(dyn Error + Send + Sync + 'static)) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    })
}

/// Retry only scheduler-level UDP timeouts, bounded by the scenario deadline.
/// The socket keeps its 25 ms timeout so migration remains responsive.
fn receive_envelope_until(
    socket: &UdpSocket,
    manifest: Option<&RosterManifest>,
    deadline: Instant,
    stage: &str,
) -> ProofResult<(Envelope, SocketAddr)> {
    loop {
        match receive_envelope(socket, manifest) {
            Ok(value) => return Ok(value),
            Err(error) if transient_receive_error(error.as_ref()) && Instant::now() < deadline => {}
            Err(error) if transient_receive_error(error.as_ref()) => {
                return Err(proof_error(format!(
                    "two-peer {stage} timed out before its required signed packet"
                )));
            }
            Err(error) => {
                return Err(proof_error(format!(
                    "two-peer {stage} receive failed: {error}"
                )))
            }
        }
    }
}

fn reject_tampered_signature(
    session: &mut SecureSession,
    envelope: &Envelope,
    source: SocketAddr,
    now_ms: u64,
) -> ProofResult<()> {
    let mut tampered = envelope.clone();
    tampered.simulation_tick = tampered.simulation_tick.saturating_add(1);
    if session.accept_with_source(&tampered, source, None, now_ms) != AcceptOutcome::BadSignature {
        return Err(proof_error("tampered signed envelope was not rejected"));
    }
    Ok(())
}

fn emit_event(
    writer: &mut TcpStream,
    node: Node,
    kind: EventKind,
    session: &SecureSession,
) -> ProofResult<()> {
    let authority = if session.state().authority() == Node::A.id()? {
        Node::A
    } else if session.state().authority() == Node::B.id()? {
        Node::B
    } else if session.state().authority() == Node::C.id()? {
        Node::C
    } else {
        return Err(proof_error("session reported an unknown authority"));
    };
    send_control(
        writer,
        &ControlMessage::Event {
            node,
            kind,
            authority,
            epoch: session.state().authority_epoch(),
        },
    )
}

fn proof_shot_command() -> ProofResult<ShotCommand> {
    Ok(ShotCommand {
        tick: SimulationTick::new(3),
        shooter_peer_id: Node::B.id()?,
        weapon_id: WeaponId::Dustwalker,
        origin: QuantizedOrigin::new(0, 1_600, 0),
        direction: QuantizedDirection::new(0, 0, -1_000_000),
        spread_seed: spurfire_protocol::shot_spread_seed(0, Node::B.id()?, 0),
        claimed_target: None,
    })
}

fn proof_shot_result() -> ProofResult<ShotResult> {
    Ok(ShotResult {
        tick: SimulationTick::new(3),
        shooter_peer_id: Node::B.id()?,
        weapon_id: WeaponId::Dustwalker,
        outcome: ShotOutcome::Miss,
        rejection_reason: None,
        resolved_direction: Some(QuantizedDirection::new(0, 0, -1_000_000)),
        target_id: None,
        hit_zone: None,
        damage: 0,
        distance_mm: None,
        eliminated: false,
    })
}

fn run_two_peer_child(
    node: Node,
    socket: UdpSocket,
    mut writer: TcpStream,
    mut session: SecureSession,
    signing_key: SigningKey,
    manifest: RosterManifest,
) -> ProofResult<()> {
    let deadline = Instant::now() + PROOF_TIMEOUT;
    match node {
        Node::A => {
            let hello = session.envelope(
                1,
                PeerPayload::Hello {
                    hostname: "signed-process-a".to_owned(),
                },
                &signing_key,
            )?;
            send_envelope(&socket, &hello, endpoint_for(&manifest, Node::B)?)?;
            let (reply, source) =
                receive_envelope_until(&socket, Some(&manifest), deadline, "peer-a rider-input")?;
            reject_tampered_signature(&mut session, &reply, source, 2)?;
            emit_event(
                &mut writer,
                node,
                EventKind::InputSignatureRejected,
                &session,
            )?;
            if session.accept_with_source(&reply, source, None, 2) != AcceptOutcome::Accepted {
                return Err(proof_error("peer A rejected peer B's signed rider input"));
            }
            if !matches!(reply.payload, PeerPayload::RiderInput { .. }) {
                return Err(proof_error("peer B reply was not rider input"));
            }
            emit_event(&mut writer, node, EventKind::SignedInputAccepted, &session)?;
            let (command, source) =
                receive_envelope_until(&socket, Some(&manifest), deadline, "peer-a shot-command")?;
            if session.accept_with_source(&command, source, None, 3) != AcceptOutcome::Accepted
                || !matches!(command.payload, PeerPayload::ShotCommand { .. })
            {
                return Err(proof_error("authority rejected shooter-bound shot command"));
            }
            emit_event(&mut writer, node, EventKind::ShotCommandAccepted, &session)?;
            for _ in 0..2 {
                let result = session.envelope(
                    3,
                    PeerPayload::ShotResult {
                        result: proof_shot_result()?,
                    },
                    &signing_key,
                )?;
                send_envelope(&socket, &result, endpoint_for(&manifest, Node::B)?)?;
            }
        }
        Node::B => {
            let (hello, source) =
                receive_envelope_until(&socket, Some(&manifest), deadline, "peer-b hello")?;
            reject_tampered_signature(&mut session, &hello, source, 1)?;
            emit_event(
                &mut writer,
                node,
                EventKind::HelloSignatureRejected,
                &session,
            )?;
            if session.accept_with_source(&hello, source, None, 1) != AcceptOutcome::Accepted {
                return Err(proof_error("peer B rejected peer A's signed hello"));
            }
            if !matches!(hello.payload, PeerPayload::Hello { .. }) {
                return Err(proof_error("peer A packet was not a hello"));
            }
            emit_event(&mut writer, node, EventKind::SignedHelloAccepted, &session)?;
            let reply = session.envelope(
                2,
                PeerPayload::RiderInput {
                    throttle_milli: 1_000,
                    steer_milli: 250,
                    buttons: RIDER_INPUT_JUMP_PRESSED,
                },
                &signing_key,
            )?;
            send_envelope(&socket, &reply, endpoint_for(&manifest, Node::A)?)?;
            let command = session.envelope(
                3,
                PeerPayload::ShotCommand {
                    command: proof_shot_command()?,
                },
                &signing_key,
            )?;
            send_envelope(&socket, &command, endpoint_for(&manifest, Node::A)?)?;
            let (first, source) =
                receive_envelope_until(&socket, Some(&manifest), deadline, "peer-b shot-result")?;
            if session.accept_with_source(&first, source, None, 4) != AcceptOutcome::Accepted {
                return Err(proof_error("peer B rejected authority shot result"));
            }
            emit_event(&mut writer, node, EventKind::ShotResultAccepted, &session)?;
            let (duplicate, source) = receive_envelope_until(
                &socket,
                Some(&manifest),
                deadline,
                "peer-b duplicate-result",
            )?;
            if session.accept_with_source(&duplicate, source, None, 5)
                != AcceptOutcome::DuplicateShotResult
            {
                return Err(proof_error(
                    "same result under a new sequence was not deduplicated",
                ));
            }
            emit_event(
                &mut writer,
                node,
                EventKind::ShotResultDuplicateRejected,
                &session,
            )?;
        }
        Node::C => return Err(proof_error("peer C is not part of the two-peer proof")),
    }
    emit_event(&mut writer, node, EventKind::Done, &session)?;
    Ok(())
}

fn start_command_reader(mut reader: BufReader<TcpStream>) -> Receiver<ProofResult<ParentCommand>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || loop {
        let result = match read_control(&mut reader) {
            Ok(Some(ControlMessage::Quiesce)) => Ok(ParentCommand::Quiesce),
            Ok(Some(ControlMessage::Stop)) => Ok(ParentCommand::Stop),
            Ok(Some(_)) => Err(proof_error("unexpected parent control message")),
            Ok(None) => Err(proof_error("parent closed before a lifecycle command")),
            Err(error) => Err(error),
        };
        let terminal = result.is_err();
        if sender.send(result).is_err() || terminal {
            return;
        }
    });
    receiver
}

fn await_kill_after_quiesce(
    commands: &Receiver<ProofResult<ParentCommand>>,
    deadline: Instant,
) -> ProofResult<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(proof_error("authority was not killed after quiescing"));
    }
    match commands.recv_timeout(remaining) {
        Ok(Ok(_)) => Err(proof_error("authority received a command after quiescing")),
        Ok(Err(error)) => Err(error),
        Err(RecvTimeoutError::Timeout) => {
            Err(proof_error("authority was not killed after quiescing"))
        }
        Err(RecvTimeoutError::Disconnected) => Err(proof_error(
            "authority control reader disconnected after quiescing",
        )),
    }
}

fn migration_payload(
    node: Node,
    session: &SecureSession,
    send_index: u64,
    tick: u64,
) -> ProofResult<PeerPayload> {
    let successor = Node::B.id()?;
    Ok(
        if session.state().authority() == successor && session.state().authority_epoch() == 2 {
            match node {
                Node::B if send_index.is_multiple_of(2) => {
                    let checkpoint = MatchCheckpoint {
                        source_epoch: 1,
                        tick,
                        riders: vec![
                            RiderCheckpoint {
                                rider_player_id: Node::B.id()?,
                                position_mm: [2_000, 0, 3_000],
                                velocity_mmps: [500, 0, 750],
                                yaw_millidegrees: 45_000,
                                stance: RiderStance::Mounted,
                                health: 72,
                                weapon_id: WeaponId::Dustwalker.as_u8(),
                                ammo_magazine: 3,
                                ammo_reserve: 18,
                                last_input_tick: tick.saturating_sub(1),
                                last_shot_tick: Some(tick.saturating_sub(2)),
                                last_command_tick: Some(tick.saturating_sub(2)),
                                shot_index: 1,
                            },
                            RiderCheckpoint {
                                rider_player_id: Node::C.id()?,
                                position_mm: [7_000, 0, 4_000],
                                velocity_mmps: [900, 0, -150],
                                yaw_millidegrees: 90_000,
                                stance: RiderStance::Mounted,
                                health: 100,
                                weapon_id: WeaponId::Dustwalker.as_u8(),
                                ammo_magazine: 5,
                                ammo_reserve: 24,
                                last_input_tick: tick.saturating_sub(1),
                                last_shot_tick: None,
                                last_command_tick: None,
                                shot_index: 0,
                            },
                        ],
                        resolved_shots: vec![(Node::B.id()?, tick.saturating_sub(2))],
                    };
                    PeerPayload::MigrationSnapshot {
                        authority: successor,
                        epoch: 2,
                        state_hash: checkpoint.hash(),
                        checkpoint,
                    }
                }
                Node::B => PeerPayload::RiderSnapshot {
                    rider_player_id: Node::B.id()?,
                    position_mm: [2_000, 0, 3_000],
                    velocity_mmps: [500, 0, 750],
                    yaw_millidegrees: 45_000,
                    stance: RiderStance::Mounted,
                },
                Node::C if send_index.is_multiple_of(3) => PeerPayload::RiderSnapshot {
                    rider_player_id: Node::C.id()?,
                    position_mm: [9_000, 0, 9_000],
                    velocity_mmps: [9_000, 0, 9_000],
                    yaw_millidegrees: 90_000,
                    stance: RiderStance::Mounted,
                },
                Node::C => PeerPayload::RiderInput {
                    throttle_milli: 900,
                    steer_milli: -150,
                    buttons: RIDER_INPUT_JUMP_PRESSED,
                },
                Node::A => PeerPayload::Heartbeat,
            }
        } else {
            PeerPayload::Heartbeat
        },
    )
}

fn run_migration_child(
    node: Node,
    socket: UdpSocket,
    mut writer: TcpStream,
    reader: BufReader<TcpStream>,
    mut session: SecureSession,
    signing_key: SigningKey,
    manifest: RosterManifest,
) -> ProofResult<()> {
    let commands = start_command_reader(reader);
    let started = Instant::now();
    let mut last_send = started;
    let mut send_index = 0_u64;
    let mut seen_peers = BTreeSet::new();
    let mut mesh_reported = false;
    let mut migrated_reported = false;
    let mut migration_signature_rejected = false;
    let mut migration_claim_accepted = false;
    let mut gameplay_signature_rejected = false;
    let mut non_authority_snapshot_rejected = false;
    let mut gameplay_accepted = false;
    let mut snapshot_accepted = false;

    loop {
        match commands.try_recv() {
            Ok(Ok(ParentCommand::Quiesce)) if node == Node::A => {
                send_control(&mut writer, &ControlMessage::Quiesced { node })?;
                // The migration loop owns the sole writer. Once the flushed
                // acknowledgment is sent, it enters a permanent no-write state
                // and only the parent's forced process kill can end the proof.
                return await_kill_after_quiesce(&commands, started + PROOF_TIMEOUT);
            }
            Ok(Ok(ParentCommand::Quiesce)) => {
                return Err(proof_error(format!(
                    "non-authority peer {node} received quiesce"
                )));
            }
            Ok(Ok(ParentCommand::Stop)) => {
                let complete = match node {
                    Node::B => {
                        mesh_reported
                            && migrated_reported
                            && gameplay_signature_rejected
                            && non_authority_snapshot_rejected
                            && gameplay_accepted
                    }
                    Node::C => {
                        mesh_reported
                            && migrated_reported
                            && migration_signature_rejected
                            && migration_claim_accepted
                            && snapshot_accepted
                    }
                    Node::A => false,
                };
                if !complete {
                    return Err(proof_error(format!(
                        "peer {node} received stop before completing its signed migration proof"
                    )));
                }
                emit_event(&mut writer, node, EventKind::Done, &session)?;
                return Ok(());
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(proof_error("lifecycle command reader disconnected"));
            }
        }

        let elapsed = started.elapsed();
        if elapsed > PROOF_TIMEOUT {
            return Err(proof_error(format!(
                "peer {node} exceeded the migration proof timeout"
            )));
        }
        let now_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        if node != Node::A {
            if let Some((successor, epoch)) = session.expire_and_migrate(now_ms) {
                if successor != Node::B.id()? || epoch != 2 {
                    return Err(proof_error(
                        "deterministic migration chose the wrong successor",
                    ));
                }
            }
            if !migrated_reported
                && session.state().authority() == Node::B.id()?
                && session.state().authority_epoch() == 2
            {
                migrated_reported = true;
                emit_event(&mut writer, node, EventKind::Migrated, &session)?;
            }
        }

        if last_send.elapsed() >= SEND_INTERVAL {
            last_send = Instant::now();
            send_index = send_index.saturating_add(1);
            let tick = now_ms / 16;
            let payload = migration_payload(node, &session, send_index, tick)?;
            let envelope = if node == Node::C
                && matches!(payload, PeerPayload::RiderSnapshot { .. })
            {
                // Deliberately bypass the outbound role builder to prove that
                // receivers reject a compromised roster member's validly
                // signed authority-only payload.
                let mut forged = session.envelope(tick, PeerPayload::Heartbeat, &signing_key)?;
                forged.payload = payload;
                session.sign(&mut forged, &signing_key)?;
                forged
            } else {
                session.envelope(tick, payload, &signing_key)?
            };
            for peer in Scenario::Migration.nodes() {
                if *peer != node {
                    send_envelope(&socket, &envelope, endpoint_for(&manifest, *peer)?)?;
                }
            }
        }

        match receive_envelope(&socket, Some(&manifest)) {
            Ok((envelope, source)) => {
                let payload = envelope.payload.clone();
                if node == Node::C
                    && session.state().authority() == Node::B.id()?
                    && session.state().authority_epoch() == 2
                    && envelope.sender == Node::B.id()?
                    && envelope.authority_epoch == 2
                    && matches!(
                        payload,
                        PeerPayload::MigrationSnapshot {
                            authority,
                            epoch: 2,
                            ..
                        } if authority == Node::B.id()?
                    )
                    && !migration_signature_rejected
                {
                    reject_tampered_signature(&mut session, &envelope, source, now_ms)?;
                    migration_signature_rejected = true;
                    emit_event(
                        &mut writer,
                        node,
                        EventKind::MigrationClaimSignatureRejected,
                        &session,
                    )?;
                }
                if node == Node::B
                    && envelope.sender == Node::C.id()?
                    && envelope.authority_epoch == 2
                    && matches!(payload, PeerPayload::RiderInput { .. })
                    && !gameplay_signature_rejected
                {
                    reject_tampered_signature(&mut session, &envelope, source, now_ms)?;
                    gameplay_signature_rejected = true;
                    emit_event(
                        &mut writer,
                        node,
                        EventKind::GameplaySignatureRejected,
                        &session,
                    )?;
                }

                if node == Node::B
                    && session.state().authority() == Node::B.id()?
                    && session.state().authority_epoch() == 2
                    && envelope.sender == Node::C.id()?
                    && envelope.authority_epoch == 2
                    && matches!(payload, PeerPayload::RiderSnapshot { .. })
                    && !non_authority_snapshot_rejected
                {
                    if session.accept_with_source(&envelope, source, None, now_ms)
                        != AcceptOutcome::InvalidPayloadRole
                    {
                        return Err(proof_error(
                            "peer B did not reject a non-authority signed snapshot",
                        ));
                    }
                    non_authority_snapshot_rejected = true;
                    emit_event(
                        &mut writer,
                        node,
                        EventKind::NonAuthoritySnapshotRejected,
                        &session,
                    )?;
                    continue;
                }

                let outcome = session.accept_with_source(&envelope, source, None, now_ms);
                if outcome == AcceptOutcome::Accepted {
                    if envelope.authority_epoch == 1 {
                        if envelope.sender == Node::A.id()? {
                            seen_peers.insert(Node::A);
                        } else if envelope.sender == Node::B.id()? {
                            seen_peers.insert(Node::B);
                        } else if envelope.sender == Node::C.id()? {
                            seen_peers.insert(Node::C);
                        }
                    }
                    if node == Node::C
                        && envelope.sender == Node::B.id()?
                        && matches!(
                            payload,
                            PeerPayload::MigrationSnapshot {
                                authority,
                                epoch: 2,
                                ..
                            } if authority == Node::B.id()?
                        )
                        && !migration_claim_accepted
                    {
                        migration_claim_accepted = true;
                        emit_event(
                            &mut writer,
                            node,
                            EventKind::SignedMigrationClaimAccepted,
                            &session,
                        )?;
                    }
                    if node == Node::B
                        && envelope.sender == Node::C.id()?
                        && envelope.authority_epoch == 2
                        && matches!(payload, PeerPayload::RiderInput { .. })
                        && !gameplay_accepted
                    {
                        gameplay_accepted = true;
                        emit_event(
                            &mut writer,
                            node,
                            EventKind::SignedGameplayAccepted,
                            &session,
                        )?;
                    }
                    if node == Node::C
                        && envelope.sender == Node::B.id()?
                        && envelope.authority_epoch == 2
                        && matches!(payload, PeerPayload::RiderSnapshot { .. })
                        && !snapshot_accepted
                    {
                        snapshot_accepted = true;
                        emit_event(
                            &mut writer,
                            node,
                            EventKind::SignedAuthoritySnapshotAccepted,
                            &session,
                        )?;
                    }
                } else if !matches!(
                    outcome,
                    AcceptOutcome::DuplicateOrReplay
                        | AcceptOutcome::StaleAuthorityEpoch
                        | AcceptOutcome::InvalidAuthorityClaim
                        | AcceptOutcome::InvalidPayloadRole
                ) {
                    return Err(proof_error(format!(
                        "peer {node} rejected a legitimate signed packet as {outcome:?}"
                    )));
                }
            }
            Err(error) => {
                if !transient_receive_error(error.as_ref()) {
                    return Err(error);
                }
            }
        }

        let expected_seen = Scenario::Migration
            .nodes()
            .iter()
            .copied()
            .filter(|peer| *peer != node)
            .collect::<BTreeSet<_>>();
        if !mesh_reported && expected_seen.is_subset(&seen_peers) {
            mesh_reported = true;
            emit_event(&mut writer, node, EventKind::Mesh, &session)?;
        }
    }
}

fn child_main(scenario: Scenario, node: Node, control: SocketAddr) -> ProofResult<()> {
    let (socket, writer, reader, session, signing_key, manifest) =
        child_handshake(scenario, node, control)?;
    match scenario {
        Scenario::TwoPeer => {
            drop(reader);
            run_two_peer_child(node, socket, writer, session, signing_key, manifest)
        }
        Scenario::Migration => {
            run_migration_child(node, socket, writer, reader, session, signing_key, manifest)
        }
    }
}

fn main() -> ProofResult<()> {
    let mut arguments = env::args().skip(1);
    match arguments.next() {
        None => {
            run_two_peer_proof()?;
            run_migration_proof()
        }
        Some(mode) if mode == "--child" => {
            let scenario = arguments
                .next()
                .ok_or_else(|| proof_error("child scenario is required"))?
                .parse::<Scenario>()
                .map_err(proof_error)?;
            let node = arguments
                .next()
                .ok_or_else(|| proof_error("child node is required"))?
                .parse::<Node>()
                .map_err(proof_error)?;
            let control = arguments
                .next()
                .ok_or_else(|| proof_error("child control address is required"))?
                .parse::<SocketAddr>()?;
            if arguments.next().is_some() {
                return Err(proof_error("unexpected child proof arguments"));
            }
            child_main(scenario, node, control)
        }
        Some(_) => Err(proof_error("usage: spurfire-local-p2p-proof")),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    struct SplitIo {
        bytes: Cursor<Vec<u8>>,
        maximum: usize,
        write_sizes: Vec<usize>,
        flushed: bool,
    }

    impl SplitIo {
        fn reader(bytes: Vec<u8>, maximum: usize) -> Self {
            Self {
                bytes: Cursor::new(bytes),
                maximum,
                write_sizes: Vec::new(),
                flushed: false,
            }
        }

        fn writer(maximum: usize) -> Self {
            Self::reader(Vec::new(), maximum)
        }
    }

    impl Read for SplitIo {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let limit = buffer.len().min(self.maximum);
            self.bytes.read(&mut buffer[..limit])
        }
    }

    impl Write for SplitIo {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.write_sizes.push(buffer.len());
            let accepted = buffer.len().min(self.maximum);
            self.bytes.get_mut().extend_from_slice(&buffer[..accepted]);
            Ok(accepted)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }

    struct GatedReader {
        bytes: Cursor<Vec<u8>>,
        first_chunk: usize,
        first_read: mpsc::Sender<()>,
        resume: Option<mpsc::Receiver<()>>,
        first_read_complete: bool,
    }

    impl Read for GatedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.first_read_complete {
                self.first_read_complete = true;
                let limit = buffer.len().min(self.first_chunk);
                let read = self.bytes.read(&mut buffer[..limit])?;
                self.first_read.send(()).unwrap();
                return Ok(read);
            }
            if let Some(resume) = self.resume.take() {
                resume.recv().unwrap();
            }
            self.bytes.read(buffer)
        }
    }

    fn ready_frame() -> (ControlMessage, Vec<u8>) {
        let message = ControlMessage::Ready {
            scenario: Scenario::Migration,
            node: Node::A,
            endpoint: "127.0.0.1:32123".parse().unwrap(),
        };
        let frame = encode_control_frame(&message).unwrap();
        (message, frame)
    }

    #[test]
    fn framed_control_round_trips_coalesced_and_split_io() {
        let (message, frame) = ready_frame();
        let mut writer = SplitIo::writer(3);
        send_control(&mut writer, &message).unwrap();
        assert!(writer.flushed);
        assert_eq!(writer.write_sizes.first(), Some(&frame.len()));
        assert_eq!(writer.bytes.get_ref(), &frame);

        let mut coalesced = frame.clone();
        coalesced.extend_from_slice(&frame);
        let mut reader = SplitIo::reader(coalesced, 1);
        assert!(matches!(
            read_control(&mut reader).unwrap(),
            Some(ControlMessage::Ready { .. })
        ));
        assert!(matches!(
            read_control(&mut reader).unwrap(),
            Some(ControlMessage::Ready { .. })
        ));
        assert!(read_control(&mut reader).unwrap().is_none());
    }

    #[test]
    fn clean_eof_is_distinct_from_every_truncated_write_boundary() {
        let (_, frame) = ready_frame();
        assert!(read_control(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap()
            .is_none());
        for boundary in 1..frame.len() {
            let error = read_control(&mut Cursor::new(&frame[..boundary]))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("truncated control frame"),
                "boundary {boundary}: {error}"
            );
        }
    }

    #[test]
    fn oversized_and_invalid_complete_frames_are_rejected() {
        let oversized = u32::try_from(MAX_CONTROL_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes();
        let error = read_control(&mut Cursor::new(oversized))
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds limit"));

        let payload = b"not-json";
        let mut invalid = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        invalid.extend_from_slice(payload);
        let error = read_control(&mut Cursor::new(invalid))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid complete control frame"));
    }

    #[test]
    fn split_frame_survives_delayed_reader_scheduling_without_sleep() {
        let (_, frame) = ready_frame();
        let (first_read_sender, first_read_receiver) = mpsc::channel();
        let (resume_sender, resume_receiver) = mpsc::channel();
        let reader = GatedReader {
            bytes: Cursor::new(frame),
            first_chunk: 1,
            first_read: first_read_sender,
            resume: Some(resume_receiver),
            first_read_complete: false,
        };
        let worker = thread::spawn(move || read_control(&mut BufReader::new(reader)));
        first_read_receiver.recv().unwrap();
        resume_sender.send(()).unwrap();
        assert!(matches!(
            worker.join().unwrap().unwrap(),
            Some(ControlMessage::Ready { .. })
        ));
    }

    #[test]
    fn post_done_and_post_quiesce_frames_are_rejected() {
        let event = |kind| ControlMessage::Event {
            node: Node::B,
            kind,
            authority: Node::B,
            epoch: 2,
        };
        let mut done = false;
        assert!(matches!(
            notice_from_control(Node::B, event(EventKind::Done), &mut done).unwrap(),
            Notice::Event(_)
        ));
        assert!(notice_from_control(Node::B, event(EventKind::Mesh), &mut done).is_err());

        let mut quiesced = false;
        assert!(matches!(
            notice_from_control(
                Node::A,
                ControlMessage::Quiesced { node: Node::A },
                &mut quiesced,
            )
            .unwrap(),
            Notice::Quiesced(Node::A)
        ));
        assert!(notice_from_control(Node::A, ControlMessage::Stop, &mut quiesced).is_err());
    }

    #[test]
    fn reader_join_fault_still_joins_every_reader() {
        let mut readers = BTreeMap::from([
            (
                Node::A,
                thread::spawn(|| -> ProofResult<()> { Err(proof_error("forced reader fault")) }),
            ),
            (Node::B, thread::spawn(|| -> ProofResult<()> { Ok(()) })),
        ]);
        let error = join_readers(&mut readers).unwrap_err().to_string();
        assert!(error.contains("forced reader fault"));
        assert!(readers.is_empty());
    }

    #[test]
    fn child_exit_statuses_are_checked_on_success_and_fault_paths() {
        let success = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let mut peers = [PeerProcess {
            node: Node::B,
            child: Some(success),
        }];
        wait_for_success(&mut peers, None).unwrap();

        let failure = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
        let mut peers = [PeerProcess {
            node: Node::C,
            child: Some(failure),
        }];
        let error = wait_for_success(&mut peers, None).unwrap_err().to_string();
        assert!(error.contains("peer c exited with"));
    }

    #[test]
    fn early_close_before_quiesce_is_fatal() {
        let (sender, receiver) = mpsc::channel();
        sender.send(Notice::Closed(Node::A)).unwrap();
        let error = wait_for_quiesced(
            Scenario::Migration,
            &receiver,
            Node::A,
            &mut BTreeSet::new(),
            CONTROL_TIMEOUT,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("peer a closed before peer a quiesced"));
    }

    #[test]
    fn missing_quiesce_acknowledgment_obeys_the_bounded_deadline() {
        let (_sender, receiver) = mpsc::channel();
        let error = wait_for_quiesced(
            Scenario::Migration,
            &receiver,
            Node::A,
            &mut BTreeSet::new(),
            Duration::ZERO,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out waiting for peer a quiesced"));
    }
}
