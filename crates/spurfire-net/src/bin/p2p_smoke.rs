use std::{env, fs, net::SocketAddr};

use ed25519_dalek::{Signer, SigningKey};
use spurfire_net::{rustscale::RustScalePeer, PeerPayload, SecureSession, SessionState};
use spurfire_protocol::{
    canonical_manifest_digest, LobbyId, PlayerId, RosterManifest, RosterManifestEntry,
    SessionPublicKey, SessionSignature,
};
use tokio::time::{sleep, Duration};
use zeroize::Zeroizing;

async fn close_with_retry(peer: &mut RustScalePeer) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..4 {
        match peer.close().await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error.to_string());
                sleep(Duration::from_millis(250)).await;
            }
        }
    }
    let error = last_error.unwrap_or_else(|| "unknown RustScale close failure".into());
    if error.contains("portmapper cleanup remains uncertain") {
        eprintln!("warning: RustScale port-mapper cleanup remained uncertain after retries; process exit will release local resources");
        Ok(())
    } else {
        Err(error)
    }
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

fn read_key(path: &str) -> Result<Zeroizing<String>, String> {
    let value = Zeroizing::new(
        fs::read_to_string(path).map_err(|error| format!("read key file: {error}"))?,
    );
    let trimmed = Zeroizing::new(value.trim().to_owned());
    if trimmed.is_empty() {
        return Err("auth key file was empty".into());
    }
    Ok(trimmed)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key_a = read_key(&argument("--key-a")?)?;
    let key_b = read_key(&argument("--key-b")?)?;
    let mut a = RustScalePeer::connect("spurfire-p2p-a", key_a, 41_641).await?;
    let mut b = RustScalePeer::connect("spurfire-p2p-b", key_b, 41_641).await?;
    sleep(Duration::from_secs(2)).await;

    let lobby = LobbyId::parse("00000000-0000-4000-8000-000000000001")?;
    let player_a = PlayerId::parse("00000000-0000-4000-8000-000000000002")?;
    let player_b = PlayerId::parse("00000000-0000-4000-8000-000000000003")?;
    let signing_a = SigningKey::from_bytes(&[1; 32]);
    let signing_b = SigningKey::from_bytes(&[2; 32]);
    let manifest_signing = SigningKey::from_bytes(&[9; 32]);
    let manifest_public = SessionPublicKey::from_bytes(manifest_signing.verifying_key().to_bytes());
    let manifest = RosterManifest {
        lobby_id: lobby,
        network_generation: 1,
        session_generation: 1,
        roster_revision: 1,
        entries: vec![
            RosterManifestEntry {
                player_id: player_a,
                session_public_key: SessionPublicKey::from_bytes(
                    signing_a.verifying_key().to_bytes(),
                ),
                tailnet_address: a.tailnet_ip(),
                application_port: a.local_addr().port(),
                node_key: None,
            },
            RosterManifestEntry {
                player_id: player_b,
                session_public_key: SessionPublicKey::from_bytes(
                    signing_b.verifying_key().to_bytes(),
                ),
                tailnet_address: b.tailnet_ip(),
                application_port: b.local_addr().port(),
                node_key: None,
            },
        ],
    };
    let manifest_signature = SessionSignature::from_bytes(
        manifest_signing
            .sign(&canonical_manifest_digest(manifest_public, &manifest))
            .to_bytes(),
    );
    let mut state_a = SessionState::new(lobby, player_a, player_a, 0);
    let mut state_b = SessionState::new(lobby, player_b, player_a, 0);
    state_a.add_peer(player_b, 0);
    state_b.add_peer(player_a, 0);
    let mut session_a = SecureSession::new(
        manifest.clone(),
        manifest_public,
        manifest_signature,
        state_a,
    )?;
    let mut session_b = SecureSession::new(manifest, manifest_public, manifest_signature, state_b)?;

    let destination_b = SocketAddr::new(b.tailnet_ip(), b.local_addr().port());
    let hello = session_a.envelope(
        1,
        PeerPayload::Hello {
            hostname: "spurfire-p2p-a".into(),
        },
        &signing_a,
    )?;
    a.send(&hello, destination_b).await?;
    let (received, source_a) = b.recv(Duration::from_secs(15)).await?;
    if session_b.accept_with_source(&received, source_a, b.node_key_for(source_a.ip()), 1)
        != spurfire_net::AcceptOutcome::Accepted
    {
        return Err("peer B rejected peer A hello".into());
    }

    let reply = session_b.envelope(
        2,
        PeerPayload::RiderInput {
            throttle_milli: 1_000,
            steer_milli: 250,
            buttons: 1,
        },
        &signing_b,
    )?;
    b.send(&reply, source_a).await?;
    let (received_reply, source_b) = a.recv(Duration::from_secs(15)).await?;
    let mut forged = received_reply.clone();
    forged.sender = player_a;
    if session_a.accept_with_source(&forged, source_b, a.node_key_for(source_b.ip()), 2)
        == spurfire_net::AcceptOutcome::Accepted
    {
        return Err("forged sender was accepted".into());
    }
    if session_a.accept_with_source(&received_reply, source_b, a.node_key_for(source_b.ip()), 2)
        != spurfire_net::AcceptOutcome::Accepted
    {
        return Err("peer A rejected peer B gameplay frame".into());
    }

    println!(
        "SPURFIRE_P2P_UDP_OK a={} b={} route_a_to_b={} route_b_to_a={}",
        a.tailnet_ip(),
        b.tailnet_ip(),
        a.route_to(b.tailnet_ip())
            .unwrap_or_else(|| "unknown".into()),
        b.route_to(a.tailnet_ip())
            .unwrap_or_else(|| "unknown".into())
    );
    close_with_retry(&mut a).await?;
    close_with_retry(&mut b).await?;
    Ok(())
}
