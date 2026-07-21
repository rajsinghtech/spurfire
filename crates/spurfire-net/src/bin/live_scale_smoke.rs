use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Instant,
};

use ed25519_dalek::{Signer, SigningKey};
use spurfire_net::{
    rustscale::{RustScalePeer, RustScaleTransportError},
    v2::{decode_m3, encode_m3, M3ActorInput, M3PeerPayloadV2, M3SecureSession},
    AcceptOutcome, SessionState,
};
use spurfire_protocol::{
    canonical_manifest_digest, LobbyId, PlayerId, RosterManifest, RosterManifestEntry,
    SessionPublicKey, SessionSignature,
};
use tokio::time::Duration;
use zeroize::Zeroizing;

const PEER_COUNT: usize = 16;
const APPLICATION_PORT: u16 = 41_643;
const CHURNED: [usize; 4] = [3, 7, 11, 15];
const LOBBY: &str = "00000000-0000-4000-8000-000000000016";

type ProofResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn proof_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::other(message.into()).into()
}

fn argument(name: &str) -> Result<String, String> {
    let mut args = env::args().skip(1);
    while let Some(value) = args.next() {
        if value == name {
            return args
                .next()
                .ok_or_else(|| format!("missing value for {name}"));
        }
    }
    Err(format!("required argument: {name}"))
}

fn player(index: usize) -> ProofResult<PlayerId> {
    Ok(PlayerId::parse(&format!(
        "00000000-0000-4000-8000-{:012}",
        index + 1
    ))?)
}

fn signing_key(index: usize) -> ProofResult<SigningKey> {
    let seed = u8::try_from(index + 1)?;
    Ok(SigningKey::from_bytes(&[seed; 32]))
}

fn read_key(path: &Path) -> ProofResult<Zeroizing<Vec<u8>>> {
    let value = Zeroizing::new(
        fs::read(path).map_err(|error| proof_error(format!("read key file: {error}")))?,
    );
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    let trimmed = Zeroizing::new(value[start..end].to_vec());
    if trimmed.is_empty() {
        return Err(proof_error("auth key file was empty"));
    }
    Ok(trimmed)
}

async fn connect_batch(
    specs: Vec<(usize, PathBuf, String)>,
) -> ProofResult<Vec<(usize, RustScalePeer)>> {
    let mut peers = Vec::with_capacity(specs.len());
    for (index, key_path, hostname) in specs {
        let mut connected = None;
        let mut last_error = None;
        for attempt in 0..3 {
            let key = read_key(&key_path)?;
            match RustScalePeer::connect(hostname.clone(), key, APPLICATION_PORT).await {
                Ok(peer) => {
                    connected = Some(peer);
                    break;
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        }
        let peer = connected.ok_or_else(|| {
            proof_error(format!(
                "peer {index} enrollment failed after retries: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "unknown RustScale error".into())
            ))
        })?;
        peers.push((index, peer));
    }
    peers.sort_unstable_by_key(|(index, _)| *index);
    Ok(peers)
}

fn signed_manifest(
    peers: &[Option<RustScalePeer>],
    roster_revision: u64,
) -> ProofResult<(RosterManifest, SessionPublicKey, SessionSignature)> {
    let manifest = RosterManifest {
        lobby_id: LobbyId::parse(LOBBY)?,
        network_generation: 1,
        session_generation: 1,
        roster_revision,
        entries: peers
            .iter()
            .enumerate()
            .map(|(index, peer)| {
                let peer = peer
                    .as_ref()
                    .ok_or_else(|| proof_error(format!("peer {index} is absent from manifest")))?;
                Ok(RosterManifestEntry {
                    player_id: player(index)?,
                    session_public_key: SessionPublicKey::from_bytes(
                        signing_key(index)?.verifying_key().to_bytes(),
                    ),
                    tailnet_address: peer.tailnet_ip(),
                    application_port: peer.local_addr().port(),
                    node_key: None,
                })
            })
            .collect::<ProofResult<Vec<_>>>()?,
    };
    let manifest_signing = SigningKey::from_bytes(&[99; 32]);
    let manifest_public = SessionPublicKey::from_bytes(manifest_signing.verifying_key().to_bytes());
    let signature = SessionSignature::from_bytes(
        manifest_signing
            .sign(&canonical_manifest_digest(manifest_public, &manifest))
            .to_bytes(),
    );
    Ok((manifest, manifest_public, signature))
}

fn secure_sessions(
    manifest: &RosterManifest,
    manifest_public: SessionPublicKey,
    manifest_signature: SessionSignature,
) -> ProofResult<Vec<M3SecureSession>> {
    let lobby = LobbyId::parse(LOBBY)?;
    let authority = player(0)?;
    (0..PEER_COUNT)
        .map(|local| {
            let mut state = SessionState::new(lobby, player(local)?, authority, 0);
            for index in 0..PEER_COUNT {
                state.add_peer(player(index)?, 0);
            }
            Ok(M3SecureSession::new(
                manifest.clone(),
                manifest_public,
                manifest_signature,
                state,
            )?)
        })
        .collect()
}

fn endpoint(peer: &RustScalePeer) -> SocketAddr {
    SocketAddr::new(peer.tailnet_ip(), peer.local_addr().port())
}

async fn wait_for_peer_visibility(peers: &[Option<RustScalePeer>]) -> ProofResult<()> {
    let started = Instant::now();
    loop {
        let mut missing = Vec::new();
        for (sender, sender_peer) in peers.iter().enumerate() {
            let sender_peer = sender_peer
                .as_ref()
                .ok_or_else(|| proof_error(format!("visibility sender {sender} is absent")))?;
            for (receiver, receiver_peer) in peers.iter().enumerate() {
                if sender == receiver {
                    continue;
                }
                let receiver_ip = receiver_peer
                    .as_ref()
                    .ok_or_else(|| {
                        proof_error(format!("visibility receiver {receiver} is absent"))
                    })?
                    .tailnet_ip();
                if sender_peer.route_to(receiver_ip).is_none() {
                    missing.push((sender, receiver));
                }
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(90) {
            missing.truncate(16);
            return Err(proof_error(format!(
                "peer visibility did not converge within 90s; first_missing={missing:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn prove_full_mesh(
    peers: &mut [Option<RustScalePeer>],
    sessions: &mut [M3SecureSession],
    tick_base: u64,
    replacement_inputs: bool,
) -> ProofResult<u64> {
    wait_for_peer_visibility(peers).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut accepted = 0_u64;
    for first in 0..PEER_COUNT {
        for second in (first + 1)..PEER_COUNT {
            prove_direction(
                peers,
                sessions,
                first,
                second,
                tick_base,
                replacement_inputs,
            )
            .await?;
            accepted += 1;
            prove_direction(
                peers,
                sessions,
                second,
                first,
                tick_base,
                replacement_inputs,
            )
            .await?;
            accepted += 1;
        }
    }
    Ok(accepted)
}

async fn prove_direction(
    peers: &mut [Option<RustScalePeer>],
    sessions: &mut [M3SecureSession],
    sender: usize,
    receiver: usize,
    tick_base: u64,
    replacement_inputs: bool,
) -> ProofResult<()> {
    let expected_sender = player(sender)?;
    let destination = endpoint(
        peers[receiver]
            .as_ref()
            .ok_or_else(|| proof_error(format!("mesh receiver {receiver} is absent")))?,
    );
    for attempt in 0_u64..5 {
        let payload = if replacement_inputs && CHURNED.contains(&sender) {
            M3PeerPayloadV2::ActorInput {
                input: M3ActorInput {
                    throttle_milli: 1_000,
                    steer_milli: i16::try_from(sender * 10)?,
                    move_x_milli: 0,
                    move_z_milli: 0,
                    buttons: 1,
                },
            }
        } else {
            M3PeerPayloadV2::Heartbeat
        };
        let envelope =
            sessions[sender].envelope(tick_base + attempt, payload, &signing_key(sender)?)?;
        let bytes = encode_m3(&envelope)?;
        peers[sender]
            .as_ref()
            .ok_or_else(|| proof_error(format!("mesh sender {sender} is absent")))?
            .send_datagram(&bytes, destination)
            .await?;

        let mut drained = 0_u64;
        loop {
            let receiver_peer = peers[receiver]
                .as_mut()
                .ok_or_else(|| proof_error(format!("mesh receiver {receiver} disappeared")))?;
            let (bytes, source) = match receiver_peer.recv_datagram(Duration::from_secs(3)).await {
                Ok(value) => value,
                Err(RustScaleTransportError::Timeout) => break,
                Err(error) => return Err(error.into()),
            };
            let received = decode_m3(&bytes)?;
            let now_ms = tick_base
                .saturating_mul(1_000)
                .saturating_add(u64::try_from(sender * PEER_COUNT + receiver)? * 100)
                .saturating_add(attempt * 10)
                .saturating_add(drained);
            drained = drained.saturating_add(1);
            let outcome = sessions[receiver].accept_with_source(
                &received,
                source,
                receiver_peer.node_key_for(source.ip()),
                now_ms,
            );
            match outcome {
                AcceptOutcome::Accepted if received.sender == expected_sender => return Ok(()),
                AcceptOutcome::Accepted
                | AcceptOutcome::DuplicateOrReplay
                | AcceptOutcome::RosterMismatch => continue,
                _ => {
                    return Err(proof_error(format!(
                        "peer {receiver} rejected mesh sender {} while expecting {sender} as {outcome:?}",
                        received.sender
                    )))
                }
            }
        }
    }
    let forward = peers[sender]
        .as_ref()
        .and_then(|peer| peer.route_to(destination.ip()))
        .unwrap_or_else(|| "unknown".into());
    let reverse = peers[receiver]
        .as_ref()
        .and_then(|peer| {
            peers[sender]
                .as_ref()
                .and_then(|sender_peer| peer.route_to(sender_peer.tailnet_ip()))
        })
        .unwrap_or_else(|| "unknown".into());
    Err(proof_error(format!(
        "mesh direction {sender}->{receiver} timed out after retries; routes={forward}/{reverse}"
    )))
}

async fn prove_leaves(
    peers: &mut [Option<RustScalePeer>],
    sessions: &mut [M3SecureSession],
) -> ProofResult<u64> {
    for sender in CHURNED {
        let envelope =
            sessions[sender].envelope(10_000, M3PeerPayloadV2::Leave, &signing_key(sender)?)?;
        let bytes = encode_m3(&envelope)?;
        let sender_peer = peers[sender]
            .as_ref()
            .ok_or_else(|| proof_error(format!("leave sender {sender} is absent")))?;
        for (receiver, receiver_peer) in peers.iter().enumerate() {
            if !CHURNED.contains(&receiver) {
                sender_peer
                    .send_datagram(
                        &bytes,
                        endpoint(receiver_peer.as_ref().ok_or_else(|| {
                            proof_error(format!("leave receiver {receiver} is absent"))
                        })?),
                    )
                    .await?;
            }
        }
    }

    let mut accepted = 0_u64;
    for receiver in 0..PEER_COUNT {
        if CHURNED.contains(&receiver) {
            continue;
        }
        let mut seen = BTreeSet::new();
        while seen.len() < CHURNED.len() {
            let receiver_peer = peers[receiver]
                .as_mut()
                .ok_or_else(|| proof_error(format!("leave receiver {receiver} disappeared")))?;
            let (bytes, source) = receiver_peer.recv_datagram(Duration::from_secs(15)).await?;
            let envelope = decode_m3(&bytes)?;
            if !matches!(envelope.payload, M3PeerPayloadV2::Leave) {
                continue;
            }
            let sender = CHURNED
                .into_iter()
                .find(|index| player(*index).is_ok_and(|value| value == envelope.sender))
                .ok_or_else(|| proof_error("leave came from a non-churn peer"))?;
            let outcome = sessions[receiver].accept_with_source(
                &envelope,
                source,
                receiver_peer.node_key_for(source.ip()),
                200_000 + accepted,
            );
            if outcome != AcceptOutcome::Accepted {
                return Err(proof_error(format!(
                    "peer {receiver} rejected leave sender {sender} as {outcome:?}"
                )));
            }
            seen.insert(sender);
            accepted += 1;
        }
    }
    Ok(accepted)
}

async fn close_peer(mut peer: RustScalePeer) {
    for _ in 0..4 {
        match peer.close().await {
            Ok(()) => return,
            Err(error)
                if error
                    .to_string()
                    .contains("portmapper cleanup remains uncertain") =>
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => {
                eprintln!("warning: RustScale close failed: {error}");
                return;
            }
        }
    }
    eprintln!("warning: RustScale port-mapper cleanup remained uncertain after retries; process exit will release local resources");
}

async fn close_all(peers: &mut [Option<RustScalePeer>]) {
    for peer in peers.iter_mut().filter_map(Option::take) {
        close_peer(peer).await;
    }
}

fn route_matrix(peers: &[Option<RustScalePeer>]) -> ProofResult<BTreeMap<String, u64>> {
    let mut routes = BTreeMap::new();
    for (sender, sender_peer) in peers.iter().enumerate() {
        let sender_peer = sender_peer
            .as_ref()
            .ok_or_else(|| proof_error(format!("route sender {sender} is absent")))?;
        for (receiver, receiver_peer) in peers.iter().enumerate() {
            if receiver == sender {
                continue;
            }
            let receiver_ip = receiver_peer
                .as_ref()
                .ok_or_else(|| proof_error(format!("route receiver {receiver} is absent")))?
                .tailnet_ip();
            let route = sender_peer.route_to(receiver_ip).ok_or_else(|| {
                proof_error(format!("route {sender}->{receiver} was not classified"))
            })?;
            if !matches!(route.as_str(), "Direct" | "Derp" | "PeerRelay") {
                return Err(proof_error(format!(
                    "route {sender}->{receiver} had unsupported class {route}"
                )));
            }
            *routes.entry(route).or_default() += 1;
        }
    }
    Ok(routes)
}

async fn run() -> ProofResult<()> {
    let key_dir = PathBuf::from(argument("--key-dir")?);
    let initial_specs = (0..PEER_COUNT)
        .map(|index| {
            (
                index,
                key_dir.join(format!("initial-{index:02}")),
                format!("spurfire-scale-{index:02}-r1"),
            )
        })
        .collect();
    let connected = connect_batch(initial_specs).await?;
    if connected.len() != PEER_COUNT {
        return Err(proof_error(format!(
            "only {} of {PEER_COUNT} initial peers connected",
            connected.len()
        )));
    }
    let mut peers: Vec<Option<RustScalePeer>> =
        std::iter::repeat_with(|| None).take(PEER_COUNT).collect();
    for (index, peer) in connected {
        peers[index] = Some(peer);
    }

    let (manifest, manifest_public, manifest_signature) = signed_manifest(&peers, 1)?;
    let old_endpoints = manifest
        .entries
        .iter()
        .map(|entry| SocketAddr::new(entry.tailnet_address, entry.application_port))
        .collect::<Vec<_>>();
    let mut sessions = secure_sessions(&manifest, manifest_public, manifest_signature)?;
    let initial_packets = prove_full_mesh(&mut peers, &mut sessions, 100, false).await?;
    let leave_packets = prove_leaves(&mut peers, &mut sessions).await?;

    for index in CHURNED {
        let peer = peers[index]
            .take()
            .ok_or_else(|| proof_error(format!("churn peer {index} was already absent")))?;
        close_peer(peer).await;
    }

    let replacement_specs = CHURNED
        .into_iter()
        .map(|index| {
            (
                index,
                key_dir.join(format!("replacement-{index:02}")),
                format!("spurfire-scale-{index:02}-r2"),
            )
        })
        .collect();
    for (index, peer) in connect_batch(replacement_specs).await? {
        peers[index] = Some(peer);
    }
    for index in CHURNED {
        let replacement = endpoint(
            peers[index]
                .as_ref()
                .ok_or_else(|| proof_error(format!("replacement peer {index} is absent")))?,
        );
        if replacement == old_endpoints[index] {
            close_all(&mut peers).await;
            return Err(proof_error(format!(
                "replacement peer {index} reused its old endpoint"
            )));
        }
    }

    let (manifest, manifest_public, manifest_signature) = signed_manifest(&peers, 2)?;
    sessions = secure_sessions(&manifest, manifest_public, manifest_signature)?;
    let revised_packets = prove_full_mesh(&mut peers, &mut sessions, 20_000, true).await?;
    let routes = route_matrix(&peers)?;
    let route_total = routes.values().sum::<u64>();
    let expected_routes = u64::try_from(PEER_COUNT * (PEER_COUNT - 1))?;
    if route_total != expected_routes {
        close_all(&mut peers).await;
        return Err(proof_error(format!(
            "route matrix had {route_total} of {expected_routes} directed paths"
        )));
    }

    println!(
        "SPURFIRE_LIVE_SCALE_OK peers={PEER_COUNT} initial_mesh_packets={initial_packets} signed_leaves={leave_packets} replacements={} roster_revision=2 revised_mesh_packets={revised_packets} replacement_inputs=accepted directed_routes={route_total} route_classes={}",
        CHURNED.len(),
        routes
            .iter()
            .map(|(route, count)| format!("{route}:{count}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    close_all(&mut peers).await;
    Ok(())
}

#[tokio::main]
async fn main() -> ProofResult<()> {
    run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_scale_identities_and_churn_set_are_exact() {
        let players = (0..PEER_COUNT)
            .map(|index| player(index).unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(players.len(), PEER_COUNT);
        assert_eq!(CHURNED, [3, 7, 11, 15]);
        assert!(CHURNED.into_iter().all(|index| index < PEER_COUNT));
    }
}
