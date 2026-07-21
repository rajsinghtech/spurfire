use std::{
    collections::BTreeMap,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration as StdDuration, Instant},
};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use spurfire_net::{
    rustscale::{RustScalePeer, RustScaleTransportError},
    v2::{
        decode_m3, encode_m3, fragment_m3_checkpoint, M3ActorInput, M3PeerPayloadV2,
        M3SecureSession, M5MatchStateV2,
    },
    AcceptOutcome, M3MatchCheckpointV2, MatchCheckpoint, RiderCheckpoint, SessionState,
};
use spurfire_protocol::{
    canonical_manifest_digest, ActorM3TickInput, BountyMatchKernel, EntityId, HorseVitalityClass,
    LobbyId, M3AuthorityBank, M3ReloadCheckpointV2, OnFootTickInput, PlayerId, QuantizedOrigin,
    RiderStance, RosterManifest, RosterManifestEntry, SessionPublicKey, SessionSignature,
    SimulationTick, WeaponId, M3_WIRE_VERSION, OBJECTIVE_CADENCE_TICKS,
};
use tokio::time::Duration;
use zeroize::Zeroizing;

const LOBBY: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_A: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_B: &str = "00000000-0000-4000-8000-000000000002";
const PLAYER_C: &str = "00000000-0000-4000-8000-000000000003";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MeshConfig {
    endpoints: BTreeMap<String, SocketAddr>,
    manifest: RosterManifest,
    manifest_public_key: SessionPublicKey,
    manifest_signature: SessionSignature,
}

const CHECKPOINT_ACK: u64 = 0x4c_49_56_45_4d_35_41_43;

fn arg(name: &str) -> Result<String, String> {
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

fn player(node: &str) -> Result<PlayerId, Box<dyn std::error::Error>> {
    Ok(PlayerId::parse(match node {
        "a" => PLAYER_A,
        "b" => PLAYER_B,
        "c" => PLAYER_C,
        _ => return Err("node must be a, b, or c".into()),
    })?)
}

fn signing_key(node: &str) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let seed = match node {
        "a" => 1,
        "b" => 2,
        "c" => 3,
        _ => return Err("node must be a, b, or c".into()),
    };
    Ok(SigningKey::from_bytes(&[seed; 32]))
}

fn source_checkpoint() -> Result<M3MatchCheckpointV2, Box<dyn std::error::Error>> {
    let players = vec![player("a")?, player("b")?, player("c")?];
    let checkpoint_tick = SimulationTick::new(OBJECTIVE_CADENCE_TICKS);
    let mut gameplay = M3AuthorityBank::new(1);
    for (index, player_id) in players.iter().copied().enumerate() {
        if !gameplay.register_actor(
            player_id,
            EntityId(100 + u64::try_from(index)?),
            HorseVitalityClass::Courser,
        ) {
            return Err("live checkpoint actor registration failed".into());
        }
        gameplay.advance_actor(
            player_id,
            ActorM3TickInput {
                tick: checkpoint_tick,
                on_foot: OnFootTickInput {
                    tick: checkpoint_tick,
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
        )?;
    }

    let mut bounty = BountyMatchKernel::new(
        1,
        0x5f_4c_49_56_45_4d_35,
        SimulationTick::new(0),
        players.clone(),
    )?;
    bounty.advance_tick(checkpoint_tick)?;
    if bounty.active_objective().is_none() {
        return Err("live checkpoint has no active objective".into());
    }
    bounty.record_damage(checkpoint_tick, player("c")?, player("a")?, 40)?;
    bounty.record_elimination(checkpoint_tick, player("b")?, player("a")?, true)?;
    bounty.record_horse_bolt(checkpoint_tick, player("b")?, player("c")?)?;
    bounty.record_mounted_long_hit(checkpoint_tick, player("b")?, 61_000)?;

    let checkpoint = M3MatchCheckpointV2 {
        wire_version: M3_WIRE_VERSION,
        combat: MatchCheckpoint {
            source_epoch: 1,
            tick: checkpoint_tick.as_u64(),
            riders: players
                .iter()
                .copied()
                .map(|rider_player_id| {
                    let fired = rider_player_id == player("b")?;
                    Ok(RiderCheckpoint {
                        rider_player_id,
                        position_mm: [0; 3],
                        velocity_mmps: [0; 3],
                        yaw_millidegrees: 0,
                        stance: RiderStance::Mounted,
                        health: 100,
                        weapon_id: WeaponId::Dustwalker.as_u8(),
                        ammo_magazine: if fired { 29 } else { 30 },
                        ammo_reserve: 120,
                        last_input_tick: checkpoint_tick.as_u64(),
                        last_shot_tick: fired.then_some(checkpoint_tick.as_u64() - 1),
                        last_command_tick: fired.then_some(checkpoint_tick.as_u64() - 1),
                        shot_index: u64::from(fired),
                    })
                })
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?,
            resolved_shots: vec![(player("b")?, checkpoint_tick.as_u64() - 1)],
        },
        gameplay: gameplay.checkpoint(),
        reloads: players
            .iter()
            .copied()
            .map(|rider_player_id| M3ReloadCheckpointV2 {
                rider_player_id,
                current_tick: Some(checkpoint_tick),
                reload_held: false,
                reload: None,
            })
            .collect(),
        next_horse_damage_sequence: 7,
        bounty: bounty.checkpoint(),
    };
    if !checkpoint.is_bounded_and_canonical() {
        return Err("live source checkpoint is noncanonical".into());
    }
    Ok(checkpoint)
}

fn continued_match_state(
    checkpoint: &M3MatchCheckpointV2,
) -> Result<M5MatchStateV2, Box<dyn std::error::Error>> {
    let mut bounty = BountyMatchKernel::restore_checkpoint(checkpoint.bounty.clone(), 2)?;
    let source_tick = SimulationTick::new(checkpoint.combat.tick);
    bounty.record_horse_bolt(source_tick, player("c")?, player("b")?)?;
    bounty.advance_tick(source_tick.saturating_add(1))?;
    Ok(M5MatchStateV2::from_snapshot(&bounty.snapshot()))
}

fn continuity_is_exact(
    checkpoint: &M3MatchCheckpointV2,
    continued: &M5MatchStateV2,
) -> Result<bool, Box<dyn std::error::Error>> {
    let source = M5MatchStateV2::from_snapshot(&checkpoint.bounty.match_state().snapshot());
    let score = |state: &M5MatchStateV2, player_id: PlayerId| {
        state
            .players
            .iter()
            .find(|row| row.player_id == player_id)
            .map(|row| row.score)
    };
    Ok(continued.authority_epoch == 2
        && continued.current_tick.as_u64() == checkpoint.combat.tick + 1
        && continued.end_tick == source.end_tick
        && continued.active_objective == source.active_objective
        && score(&source, player("b")?) == Some(150)
        && score(&source, player("c")?) == Some(50)
        && score(continued, player("b")?) == Some(150)
        && score(continued, player("c")?) == Some(65))
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)
}

fn wait_for(path: &Path, timeout: StdDuration) -> Result<(), String> {
    let started = Instant::now();
    while !path.exists() {
        if started.elapsed() >= timeout {
            return Err(format!("timed out waiting for {}", path.display()));
        }
        thread::sleep(StdDuration::from_millis(10));
    }
    Ok(())
}

async fn close_best_effort(peer: &mut RustScalePeer) {
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
            Err(_) => return,
        }
    }
}

async fn child_main() -> Result<(), Box<dyn std::error::Error>> {
    let node = arg("--node")?;
    let directory = PathBuf::from(arg("--dir")?);
    let raw_key = Zeroizing::new(fs::read(arg("--key")?)?);
    let start = raw_key
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(raw_key.len());
    let end = raw_key
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    let key = Zeroizing::new(raw_key[start..end].to_vec());
    let mut peer = RustScalePeer::connect(format!("spurfire-migrate-{node}"), key, 41_642).await?;
    let endpoint = SocketAddr::new(peer.tailnet_ip(), peer.local_addr().port());
    write_atomic(
        &directory.join(format!("ready-{node}")),
        endpoint.to_string().as_bytes(),
    )?;

    let config_path = directory.join("mesh.json");
    wait_for(&config_path, StdDuration::from_secs(90))?;
    let config: MeshConfig = serde_json::from_slice(&fs::read(config_path)?)?;
    let lobby = LobbyId::parse(LOBBY)?;
    let local = player(&node)?;
    let authority = PlayerId::parse(PLAYER_A)?;
    let mut state = SessionState::new(lobby, local, authority, 0);
    for name in ["a", "b", "c"] {
        state.add_peer(player(name)?, 0);
    }
    let mut session = M3SecureSession::new(
        config.manifest.clone(),
        config.manifest_public_key,
        config.manifest_signature,
        state,
    )?;
    let signing_key = signing_key(&node)?;
    let checkpoint = source_checkpoint()?;
    let continued = continued_match_state(&checkpoint)?;
    if !continuity_is_exact(&checkpoint, &continued)? {
        return Err("live continued-state fixture is inconsistent".into());
    }

    let started = Instant::now();
    let mut last_send = StdDuration::ZERO;
    let mut saw_authority = node == "a";
    let mut saw_other_survivor = node == "a";
    let mut migration_announced = false;
    let mut gameplay_continued = false;
    let mut fragments_sent = false;
    let mut checkpoint_installed = false;
    let mut checkpoint_acknowledged = false;
    let mut score_continued = false;

    loop {
        if directory.join("stop").exists() {
            break;
        }
        let elapsed = started.elapsed();
        let now_ms = elapsed.as_millis().try_into().unwrap_or(u64::MAX);
        let send_interval = StdDuration::from_millis(20);
        if elapsed.saturating_sub(last_send) >= send_interval {
            last_send = elapsed;
            if node == "b" && migration_announced && !fragments_sent {
                for payload in fragment_m3_checkpoint(player("b")?, 2, &checkpoint)? {
                    let envelope =
                        session.envelope(checkpoint.combat.tick, payload, &signing_key)?;
                    let bytes = encode_m3(&envelope)?;
                    peer.send_datagram(&bytes, config.endpoints["c"]).await?;
                }
                fragments_sent = true;
            } else {
                let payload = if node == "b" && checkpoint_acknowledged {
                    M3PeerPayloadV2::MatchState {
                        state: continued.clone(),
                    }
                } else if node == "c" && migration_announced {
                    M3PeerPayloadV2::ActorInput {
                        input: M3ActorInput {
                            throttle_milli: 1_000,
                            steer_milli: 200,
                            move_x_milli: 0,
                            move_z_milli: 0,
                            buttons: 1,
                        },
                    }
                } else {
                    M3PeerPayloadV2::Heartbeat
                };
                let tick = if matches!(payload, M3PeerPayloadV2::MatchState { .. }) {
                    continued.current_tick.as_u64()
                } else {
                    now_ms / 16
                };
                let envelope = session.envelope(tick, payload, &signing_key)?;
                let bytes = encode_m3(&envelope)?;
                for (name, destination) in &config.endpoints {
                    if name != &node {
                        peer.send_datagram(&bytes, *destination).await?;
                    }
                }
            }
        }

        match peer.recv_datagram(Duration::from_millis(10)).await {
            Ok((bytes, source)) => {
                let envelope = decode_m3(&bytes)?;
                let sender = envelope.sender;
                let payload = envelope.payload.clone();
                let outcome = session.accept_with_source(
                    &envelope,
                    source,
                    peer.node_key_for(source.ip()),
                    now_ms,
                );
                if outcome == AcceptOutcome::Accepted {
                    if sender == PlayerId::parse(PLAYER_A)? {
                        saw_authority = true;
                    }
                    if (node == "b" && sender == PlayerId::parse(PLAYER_C)?)
                        || (node == "c" && sender == PlayerId::parse(PLAYER_B)?)
                    {
                        saw_other_survivor = true;
                    }
                    if node == "b"
                        && session.state().authority_epoch() >= 2
                        && matches!(payload, M3PeerPayloadV2::ActorInput { .. })
                    {
                        gameplay_continued = true;
                        write_atomic(&directory.join("continued-b"), b"ok")?;
                    }
                    if node == "c"
                        && matches!(payload, M3PeerPayloadV2::MigrationFragment { .. })
                        && !checkpoint_installed
                    {
                        if session.installed_checkpoint() != Some(&checkpoint) {
                            return Err("live receiver installed a non-exact checkpoint".into());
                        }
                        checkpoint_installed = true;
                        write_atomic(&directory.join("checkpoint-c"), b"ok")?;
                        let acknowledgment = session.envelope(
                            checkpoint.combat.tick,
                            M3PeerPayloadV2::Probe {
                                nonce: CHECKPOINT_ACK,
                                reply: true,
                            },
                            &signing_key,
                        )?;
                        let bytes = encode_m3(&acknowledgment)?;
                        peer.send_datagram(&bytes, config.endpoints["b"]).await?;
                    }
                    if node == "b"
                        && matches!(
                            payload,
                            M3PeerPayloadV2::Probe {
                                nonce: CHECKPOINT_ACK,
                                reply: true,
                            }
                        )
                    {
                        checkpoint_acknowledged = true;
                    }
                    if node == "c"
                        && matches!(payload, M3PeerPayloadV2::MatchState { .. })
                        && !score_continued
                    {
                        let M3PeerPayloadV2::MatchState { state } = &payload else {
                            unreachable!("matched MatchState")
                        };
                        if !checkpoint_installed || !continuity_is_exact(&checkpoint, state)? {
                            return Err(
                                "live M5 score, clock, or objective continuity failed".into()
                            );
                        }
                        score_continued = true;
                        gameplay_continued = true;
                        write_atomic(&directory.join("score-c"), b"ok")?;
                    }
                } else if !matches!(
                    outcome,
                    AcceptOutcome::PendingMigration
                        | AcceptOutcome::DuplicateOrReplay
                        | AcceptOutcome::StaleAuthorityEpoch
                        | AcceptOutcome::InvalidAuthorityClaim
                        | AcceptOutcome::InvalidPayloadRole
                ) {
                    return Err(
                        format!("node {node} rejected live wire-2 packet as {outcome:?}").into(),
                    );
                }
            }
            Err(RustScaleTransportError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }

        if saw_authority && saw_other_survivor {
            write_atomic(&directory.join(format!("mesh-{node}")), b"ok")?;
        }
        if node != "a" {
            if let Some((successor, epoch)) = session.expire_and_migrate(now_ms) {
                if successor == PlayerId::parse(PLAYER_B)? && epoch == 2 {
                    migration_announced = true;
                    write_atomic(&directory.join(format!("migrated-{node}")), b"ok")?;
                }
            }
            if session.state().authority() == PlayerId::parse(PLAYER_B)?
                && session.state().authority_epoch() >= 2
            {
                migration_announced = true;
                write_atomic(&directory.join(format!("migrated-{node}")), b"ok")?;
            }
        }
        let complete = if node == "b" {
            migration_announced && fragments_sent && checkpoint_acknowledged && gameplay_continued
        } else if node == "c" {
            migration_announced && checkpoint_installed && score_continued && gameplay_continued
        } else {
            true
        };
        if elapsed > StdDuration::from_secs(45) && !complete {
            return Err(
                format!(
                    "node {node} incomplete live migration: migrated={migration_announced} fragments={fragments_sent} installed={checkpoint_installed} ack={checkpoint_acknowledged} score={score_continued} gameplay={gameplay_continued}"
                )
                .into(),
            );
        }
    }
    close_best_effort(&mut peer).await;
    Ok(())
}

fn spawn_child(
    node: &str,
    key: &str,
    directory: &Path,
) -> Result<Child, Box<dyn std::error::Error>> {
    Ok(Command::new(env::current_exe()?)
        .arg("--child")
        .arg("--node")
        .arg(node)
        .arg("--key")
        .arg(key)
        .arg("--dir")
        .arg(directory)
        .stdin(Stdio::null())
        .spawn()?)
}

fn parent_main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(arg("--dir")?);
    fs::create_dir_all(&directory)?;
    let keys = [arg("--key-a")?, arg("--key-b")?, arg("--key-c")?];
    let mut a = spawn_child("a", &keys[0], &directory)?;
    let mut b = spawn_child("b", &keys[1], &directory)?;
    let mut c = spawn_child("c", &keys[2], &directory)?;

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut endpoints: BTreeMap<String, SocketAddr> = BTreeMap::new();
        for node in ["a", "b", "c"] {
            let path = directory.join(format!("ready-{node}"));
            wait_for(&path, StdDuration::from_secs(90))?;
            endpoints.insert(node.to_owned(), fs::read_to_string(path)?.parse()?);
        }
        let manifest = RosterManifest {
            lobby_id: LobbyId::parse(LOBBY)?,
            network_generation: 1,
            session_generation: 1,
            roster_revision: 1,
            entries: ["a", "b", "c"]
                .into_iter()
                .map(|node| {
                    let endpoint = endpoints[node];
                    Ok(RosterManifestEntry {
                        player_id: player(node)?,
                        session_public_key: SessionPublicKey::from_bytes(
                            signing_key(node)?.verifying_key().to_bytes(),
                        ),
                        tailnet_address: endpoint.ip(),
                        application_port: endpoint.port(),
                        node_key: None,
                    })
                })
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?,
        };
        let manifest_signing = SigningKey::from_bytes(&[9; 32]);
        let manifest_public_key =
            SessionPublicKey::from_bytes(manifest_signing.verifying_key().to_bytes());
        let manifest_signature = SessionSignature::from_bytes(
            manifest_signing
                .sign(&canonical_manifest_digest(manifest_public_key, &manifest))
                .to_bytes(),
        );
        write_atomic(
            &directory.join("mesh.json"),
            &serde_json::to_vec(&MeshConfig {
                endpoints,
                manifest,
                manifest_public_key,
                manifest_signature,
            })?,
        )?;
        wait_for(&directory.join("mesh-b"), StdDuration::from_secs(15))?;
        wait_for(&directory.join("mesh-c"), StdDuration::from_secs(15))?;

        // Simulate abrupt authority process loss: no Leave packet and no graceful close.
        a.kill()?;
        let failover_started = Instant::now();
        let _ = a.wait();
        wait_for(&directory.join("migrated-b"), StdDuration::from_secs(12))?;
        wait_for(&directory.join("migrated-c"), StdDuration::from_secs(12))?;
        wait_for(&directory.join("checkpoint-c"), StdDuration::from_secs(20))?;
        wait_for(&directory.join("score-c"), StdDuration::from_secs(20))?;
        wait_for(&directory.join("continued-b"), StdDuration::from_secs(20))?;
        let failover_ms = u64::try_from(failover_started.elapsed().as_millis())?;
        if failover_ms >= 3_000 {
            return Err(format!("live failover exceeded 3s: {failover_ms}ms").into());
        }
        write_atomic(&directory.join("stop"), b"ok")?;
        if !b.wait()?.success() || !c.wait()?.success() {
            return Err("surviving peer process failed".into());
        }
        println!(
            "SPURFIRE_MIGRATION_OK authority=a successor=b epoch=2 continued_play=true checkpoint=complete_m3_m5 score_continuity=true clock_continuity=true objective_continuity=true failover_ms={failover_ms}"
        );
        Ok(())
    })();

    if result.is_err() {
        let _ = a.kill();
        let _ = b.kill();
        let _ = c.kill();
        let _ = a.wait();
        let _ = b.wait();
        let _ = c.wait();
    }
    result
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().any(|value| value == "--child") {
        child_main().await
    } else {
        parent_main()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_fixture_has_exact_continuity_and_bounded_fragments() {
        let checkpoint = source_checkpoint().unwrap();
        let continued = continued_match_state(&checkpoint).unwrap();
        assert!(continuity_is_exact(&checkpoint, &continued).unwrap());
        let fragments = fragment_m3_checkpoint(player("b").unwrap(), 2, &checkpoint).unwrap();
        println!("live_fixture_fragments={}", fragments.len());
        assert!(!fragments.is_empty());
    }
}
