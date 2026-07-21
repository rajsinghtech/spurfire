use std::{
    collections::VecDeque,
    error::Error,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    thread,
};

use ed25519_dalek::{Signer, SigningKey};
use spurfire_net::{
    replication::{RiderState, SnapshotBuffer},
    v2::{
        encode_m3, fragment_m5_match_state, M3ActorInput, M3ActorSnapshot, M3HorseSnapshot,
        M3PeerPayloadV2, M3SecureSession, M5MatchStateV2, M5ScoreRowV2,
    },
    AcceptOutcome, SessionState, MAX_DATAGRAM_BYTES,
};
use spurfire_protocol::{
    canonical_manifest_digest, EntityId, HorseVitalityClass, HorseVitalityState, LobbyId,
    M3ActorStance, PlayerId, QuantizedDirection, QuantizedOrigin, RecallState, RiderSnapshot,
    RiderStance, RidingState, RosterManifest, RosterManifestEntry, SessionPublicKey,
    SessionSignature, ShotCommand, ShotOutcome, SimulationTick, TargetDefinition,
    TargetPoseSnapshot, TargetRegistry, TeamId, WeaponId, BOUNTY_RUN_DURATION_TICKS,
};
use spurfire_protocol::{CombatAuthority, MAX_M3_AUTHORITY_ACTORS};

const CASES: [u8; 4] = [6, 8, 12, 16];
const TICK_RATE: u64 = 60;
const ACTOR_INTERVAL_TICKS: u64 = 3;
const MATCH_STATE_INTERVAL_TICKS: u64 = 30;
const SOAK_TICKS: u64 = BOUNTY_RUN_DURATION_TICKS;
const MAX_MODELED_DESYNC_TICKS: u64 = 12;

type ProofResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn proof_error(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    io::Error::other(message.into()).into()
}

fn player(index: u8) -> ProofResult<PlayerId> {
    Ok(PlayerId::parse(&format!(
        "00000000-0000-4000-8000-{:012}",
        u64::from(index) + 1
    ))?)
}

fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes(&[index.saturating_add(1); 32])
}

fn endpoint(case: u8, index: u8) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(100, 64, case, index.saturating_add(1))),
        41_700 + u16::from(index),
    )
}

fn sessions(case: u8) -> ProofResult<Vec<M3SecureSession>> {
    let lobby = LobbyId::parse(&format!(
        "00000000-0000-4000-8000-{:012}",
        100 + u64::from(case)
    ))?;
    let entries = (0..case)
        .map(|index| {
            let endpoint = endpoint(case, index);
            Ok(RosterManifestEntry {
                player_id: player(index)?,
                session_public_key: SessionPublicKey::from_bytes(
                    signing_key(index).verifying_key().to_bytes(),
                ),
                tailnet_address: endpoint.ip(),
                application_port: endpoint.port(),
                node_key: None,
            })
        })
        .collect::<ProofResult<Vec<_>>>()?;
    let manifest = RosterManifest {
        lobby_id: lobby,
        network_generation: 1,
        session_generation: 1,
        roster_revision: 1,
        entries,
    };
    let server = SigningKey::from_bytes(&[99; 32]);
    let server_public = SessionPublicKey::from_bytes(server.verifying_key().to_bytes());
    let signature = SessionSignature::from_bytes(
        server
            .sign(&canonical_manifest_digest(server_public, &manifest))
            .to_bytes(),
    );
    (0..case)
        .map(|local| {
            let mut state = SessionState::new(lobby, player(local)?, player(0)?, 0);
            for index in 0..case {
                state.add_peer(player(index)?, 0);
            }
            Ok(M3SecureSession::new(
                manifest.clone(),
                server_public,
                signature,
                state,
            )?)
        })
        .collect()
}

fn actor_snapshot(index: u8, tick: u64) -> ProofResult<M3ActorSnapshot> {
    let x = i32::try_from((tick % 30_000) * 20)?.saturating_add(i32::from(index) * 4_000);
    let horse_class = match index % 3 {
        0 => HorseVitalityClass::Courser,
        1 => HorseVitalityClass::Warhorse,
        _ => HorseVitalityClass::Mustang,
    };
    Ok(M3ActorSnapshot {
        rider_player_id: player(index)?,
        rider_position_mm: [x, 0, i32::from(index) * 2_000],
        rider_velocity_mmps: [1_200, 0, 0],
        rider_yaw_millidegrees: i32::from(index) * 10_000,
        stance: M3ActorStance::Mounted,
        rider_health: 100,
        stamina_ticks: 0,
        horse: M3HorseSnapshot {
            entity_id: EntityId(10_000 + u64::from(index)),
            class: horse_class,
            state: HorseVitalityState::Available,
            position_mm: [x, 0, i32::from(index) * 2_000],
            velocity_mmps: [1_200, 0, 0],
            yaw_millidegrees: i32::from(index) * 10_000,
            health: horse_class.max_health(),
            bolt_away_direction: QuantizedDirection::new(1_000_000, 0, 0),
        },
        recall_state: RecallState::HorsePresent,
        recall_ready_tick: None,
        spur_meter: u8::try_from(tick / 180 % 101)?,
        charge_started_tick: None,
        charge_end_tick: None,
    })
}

fn match_state(case: u8, tick: u64) -> ProofResult<M5MatchStateV2> {
    let finished = tick >= SOAK_TICKS;
    let players = (0..case)
        .map(|index| {
            let score = u32::from(index) * 10;
            Ok(M5ScoreRowV2 {
                player_id: player(index)?,
                score,
                eliminations: u16::from(index),
                assists: 0,
                deaths: 0,
                alive: true,
                respawn_at_tick: None,
                respawn_speed_buff_end_tick: None,
                horse_buff_end_tick: None,
                score_breakdown: finished.then_some([score, 0, 0, 0, 0, 0, 0, 0]),
            })
        })
        .collect::<ProofResult<Vec<_>>>()?;
    Ok(M5MatchStateV2 {
        authority_epoch: 1,
        lobby_seed: 0x53_43_41_4c_45,
        current_tick: SimulationTick::new(tick),
        end_tick: SimulationTick::new(SOAK_TICKS),
        players,
        active_reveal: None,
        active_objective: None,
        finished,
        winner: finished.then_some(player(case - 1)?),
    })
}

fn accept_packet(
    receiver: &mut M3SecureSession,
    envelope: &spurfire_net::v2::M3EnvelopeV2,
    case: u8,
    sender_index: u8,
    now_ms: u64,
) -> ProofResult<()> {
    let outcome = receiver.accept_with_source(envelope, endpoint(case, sender_index), None, now_ms);
    if outcome != AcceptOutcome::Accepted {
        return Err(proof_error(format!(
            "case {case} rejected authority packet as {outcome:?}"
        )));
    }
    Ok(())
}

fn presentation_soak(case: u8) -> ProofResult<u64> {
    let mut buffer = SnapshotBuffer::new(60, 600);
    let mut pending = VecDeque::new();
    let mut last_delivered_tick = 0_u64;
    let mut max_age_ticks = 0_u64;
    for tick in (0..SOAK_TICKS).step_by(ACTOR_INTERVAL_TICKS as usize) {
        while pending
            .front()
            .is_some_and(|(delivery_tick, _): &(u64, RiderState)| *delivery_tick <= tick)
        {
            let (_, state) = pending.pop_front().expect("front exists");
            last_delivered_tick = state.tick;
            if !buffer.push(state) {
                return Err(proof_error("presentation buffer rejected monotonic state"));
            }
        }
        let frame = tick / ACTOR_INTERVAL_TICKS;
        let dropped = frame % 23 == 7;
        if !dropped {
            let forced_relay = (18_000..21_600).contains(&tick);
            let latency_ticks = if forced_relay { 6 } else { 2 };
            pending.push_back((
                tick + latency_ticks,
                RiderState {
                    tick,
                    position_m: [tick as f32 * 0.02, 0.0, f32::from(case)],
                    velocity_mps: [1.2, 0.0, 0.0],
                    yaw_degrees: tick as f32 / 60.0,
                    stance: RiderStance::Mounted,
                },
            ));
        }
        if buffer.latest_tick().is_some() {
            let render_tick = tick.saturating_sub(6);
            let sample = buffer
                .sample(render_tick as f64)
                .ok_or_else(|| proof_error("presentation sample disappeared"))?;
            if !sample
                .state
                .position_m
                .iter()
                .all(|value| value.is_finite())
            {
                return Err(proof_error("presentation sample became non-finite"));
            }
            max_age_ticks = max_age_ticks.max(render_tick.saturating_sub(last_delivered_tick));
        }
    }
    if max_age_ticks > MAX_MODELED_DESYNC_TICKS {
        return Err(proof_error(format!(
            "case {case} modeled presentation age exceeded 200ms: {max_age_ticks} ticks"
        )));
    }
    Ok(max_age_ticks * 1_000 / TICK_RATE)
}

fn exercise_churn_and_failover(case: u8, sessions: &mut [M3SecureSession]) -> ProofResult<()> {
    let authority = 0_usize;
    for index in (2..usize::from(case)).step_by(3) {
        let leave = sessions[index].envelope(
            SOAK_TICKS,
            M3PeerPayloadV2::Leave,
            &signing_key(u8::try_from(index)?),
        )?;
        accept_packet(
            &mut sessions[authority],
            &leave,
            case,
            u8::try_from(index)?,
            900_500 + u64::try_from(index)?,
        )?;
        let rejoin = sessions[index].envelope(
            SOAK_TICKS + 1,
            M3PeerPayloadV2::ActorInput {
                input: M3ActorInput {
                    throttle_milli: 800,
                    steer_milli: 100,
                    move_x_milli: 0,
                    move_z_milli: 0,
                    buttons: 0,
                },
            },
            &signing_key(u8::try_from(index)?),
        )?;
        accept_packet(
            &mut sessions[authority],
            &rejoin,
            case,
            u8::try_from(index)?,
            901_000 + u64::try_from(index)?,
        )?;
    }

    for sender_index in 1..usize::from(case) {
        let heartbeat = sessions[sender_index].envelope(
            SOAK_TICKS + 2,
            M3PeerPayloadV2::Heartbeat,
            &signing_key(u8::try_from(sender_index)?),
        )?;
        for (receiver_index, receiver) in sessions
            .iter_mut()
            .enumerate()
            .take(usize::from(case))
            .skip(1)
        {
            if receiver_index == sender_index {
                continue;
            }
            let outcome = receiver.accept_with_source(
                &heartbeat,
                endpoint(case, u8::try_from(sender_index)?),
                None,
                902_000,
            );
            if outcome != AcceptOutcome::Accepted {
                return Err(proof_error(format!(
                    "case {case} mesh heartbeat rejected as {outcome:?}"
                )));
            }
        }
    }
    let expected = player(1)?;
    for session in sessions.iter_mut().skip(1) {
        if session.expire_and_migrate(903_001) != Some((expected, 2)) {
            return Err(proof_error(format!(
                "case {case} survivors did not converge on epoch-2 successor"
            )));
        }
    }
    Ok(())
}

fn run_case(case: u8) -> ProofResult<()> {
    if usize::from(case) > MAX_M3_AUTHORITY_ACTORS {
        return Err(proof_error("scale case exceeds protocol actor bound"));
    }
    let mut sessions = sessions(case)?;
    let mut full_snapshots = 0_u64;
    let mut delta_snapshots = 0_u64;
    let mut match_datagrams = 0_u64;
    let mut max_datagram = 0_usize;

    for tick in (0..SOAK_TICKS).step_by(ACTOR_INTERVAL_TICKS as usize) {
        for actor_index in 0..case {
            let envelope = sessions[0].envelope(
                tick,
                M3PeerPayloadV2::ActorSnapshot {
                    snapshot: actor_snapshot(actor_index, tick)?,
                },
                &signing_key(0),
            )?;
            match envelope.payload {
                M3PeerPayloadV2::ActorSnapshot { .. } => full_snapshots += 1,
                M3PeerPayloadV2::ActorSnapshotDelta { .. } => delta_snapshots += 1,
                _ => return Err(proof_error("actor stream emitted the wrong payload")),
            }
            let encoded = encode_m3(&envelope)?;
            max_datagram = max_datagram.max(encoded.len());
            let receiver_index = usize::from(actor_index % (case - 1)) + 1;
            accept_packet(
                &mut sessions[receiver_index],
                &envelope,
                case,
                0,
                tick * 1_000 / TICK_RATE,
            )?;
        }

        if tick % MATCH_STATE_INTERVAL_TICKS == 0 {
            let state = match_state(case, tick)?;
            let receiver_index =
                usize::try_from(tick / MATCH_STATE_INTERVAL_TICKS)? % (usize::from(case) - 1) + 1;
            let full = sessions[0].envelope(
                tick,
                M3PeerPayloadV2::MatchState {
                    state: state.clone(),
                },
                &signing_key(0),
            )?;
            if let Ok(encoded) = encode_m3(&full) {
                max_datagram = max_datagram.max(encoded.len());
                match_datagrams += 1;
                accept_packet(
                    &mut sessions[receiver_index],
                    &full,
                    case,
                    0,
                    tick * 1_000 / TICK_RATE,
                )?;
            } else {
                let payloads = fragment_m5_match_state(&state)?;
                for (fragment_index, payload) in payloads.into_iter().enumerate() {
                    let fragment = sessions[0].envelope(tick, payload, &signing_key(0))?;
                    let encoded = encode_m3(&fragment)?;
                    max_datagram = max_datagram.max(encoded.len());
                    match_datagrams += 1;
                    let outcome = sessions[receiver_index].accept_with_source(
                        &fragment,
                        endpoint(case, 0),
                        None,
                        tick * 1_000 / TICK_RATE,
                    );
                    let final_fragment = fragment_index + 1
                        == usize::from(match fragment.payload {
                            M3PeerPayloadV2::MatchStateFragment { fragment_count, .. } => {
                                fragment_count
                            }
                            _ => return Err(proof_error("expected MatchState fragment")),
                        });
                    let expected = if final_fragment {
                        AcceptOutcome::Accepted
                    } else {
                        AcceptOutcome::PendingFragment
                    };
                    if outcome != expected {
                        return Err(proof_error(format!(
                            "case {case} MatchState fragment returned {outcome:?}, expected {expected:?}"
                        )));
                    }
                }
            }
        }
    }

    let final_tick = SOAK_TICKS;
    let final_state = match_state(case, final_tick)?;
    let payloads = fragment_m5_match_state(&final_state)?;
    let receiver_index = usize::from(case - 1);
    for (index, payload) in payloads.iter().cloned().enumerate() {
        let fragment = sessions[0].envelope(final_tick, payload, &signing_key(0))?;
        let encoded = encode_m3(&fragment)?;
        max_datagram = max_datagram.max(encoded.len());
        match_datagrams += 1;
        let outcome = sessions[receiver_index].accept_with_source(
            &fragment,
            endpoint(case, 0),
            None,
            900_000,
        );
        let expected = if index + 1 == payloads.len() {
            AcceptOutcome::Accepted
        } else {
            AcceptOutcome::PendingFragment
        };
        if outcome != expected {
            return Err(proof_error(format!(
                "case {case} final MatchState fragment returned {outcome:?}"
            )));
        }
    }
    if max_datagram > MAX_DATAGRAM_BYTES || full_snapshots == 0 || delta_snapshots == 0 {
        return Err(proof_error(format!(
            "case {case} invalid stream totals or MTU: full={full_snapshots} delta={delta_snapshots} mtu={max_datagram}"
        )));
    }
    let max_desync_ms = presentation_soak(case)?;
    exercise_churn_and_failover(case, &mut sessions)?;
    println!(
        "SPURFIRE_LOCAL_SCALE_CASE_OK peers={case} virtual_minutes=15 actor_hz=20 match_state_hz=2 match_datagrams={match_datagrams} mtu_max={max_datagram} modeled_desync_ms={max_desync_ms} churn=reconnected failover=epoch2"
    );
    Ok(())
}

fn bot_duel_fairness() -> ProofResult<f64> {
    let shooter = player(0)?;
    let muzzle = QuantizedOrigin::new(0, 1_600, 0);
    let mut authority_hits = 0_u32;
    let mut peer_hits = 0_u32;
    let trials = 256_u64;
    for trial in 0..trials {
        let mut base = CombatAuthority::new(60, trial)?;
        if !base.register_shooter(shooter, WeaponId::Dustwalker) {
            return Err(proof_error("bot duel shooter registration failed"));
        }
        let command = ShotCommand {
            tick: SimulationTick::new(100),
            shooter_peer_id: shooter,
            weapon_id: WeaponId::Dustwalker,
            origin: muzzle,
            direction: QuantizedDirection::new(0, 0, -1_000_000),
            spread_seed: base
                .expected_spread_seed(shooter)
                .ok_or_else(|| proof_error("bot duel spread seed missing"))?,
            claimed_target: None,
        };
        let rider = RiderSnapshot {
            tick: SimulationTick::new(100),
            shooter_peer_id: shooter,
            muzzle_origin: muzzle,
            team_id: TeamId(1),
            riding: RidingState::default(),
        };
        let target = TargetDefinition {
            entity_id: EntityId(77),
            owner_peer_id: None,
            team_id: TeamId(2),
            max_health: 100,
        };
        let pose = |tick, z_mm| TargetPoseSnapshot {
            tick: SimulationTick::new(tick),
            entity_id: EntityId(77),
            stance: RiderStance::Mounted,
            body_center: QuantizedOrigin::new(0, 900, z_mm),
            body_half_height_mm: 400,
            body_radius_mm: 300,
            head_center: QuantizedOrigin::new(0, 1_600, z_mm),
            head_radius_mm: 200,
            active: true,
        };

        let mut authority_registry = TargetRegistry::new(60)?;
        authority_registry.register(target)?;
        authority_registry.record_pose(pose(100, -40_000))?;
        let authority_shot = base.clone().validate_shot(
            &command,
            SimulationTick::new(100),
            rider,
            &mut authority_registry,
        );
        authority_hits += u32::from(authority_shot.result.outcome == ShotOutcome::Hit);

        let mut peer_registry = TargetRegistry::new(60)?;
        peer_registry.register(target)?;
        peer_registry.record_pose(pose(100, -40_000))?;
        peer_registry.record_pose(pose(106, -37_600))?;
        let peer_shot = base.validate_shot(
            &command,
            SimulationTick::new(106),
            rider,
            &mut peer_registry,
        );
        peer_hits += u32::from(peer_shot.result.outcome == ShotOutcome::Hit);
    }
    let authority_rate = f64::from(authority_hits) / trials as f64;
    let peer_rate = f64::from(peer_hits) / trials as f64;
    let gap_percent = (authority_rate - peer_rate).abs() * 100.0;
    if gap_percent >= 5.0 {
        return Err(proof_error(format!(
            "modeled authority/peer hit-rate gap was {gap_percent:.2}%"
        )));
    }
    Ok(gap_percent)
}

fn main() -> ProofResult<()> {
    let results = thread::scope(|scope| {
        let handles = CASES
            .into_iter()
            .map(|case| scope.spawn(move || run_case(case)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| proof_error("scale soak worker panicked"))?
            })
            .collect::<Vec<_>>()
    });
    for result in results {
        result?;
    }
    let fairness_gap = bot_duel_fairness()?;
    println!(
        "SPURFIRE_LOCAL_SCALE_SOAK_OK cases=6,8,12,16 virtual_minutes=15 packet_loss=modeled forced_relay=modeled fairness_gap_percent={fairness_gap:.2}"
    );
    Ok(())
}
