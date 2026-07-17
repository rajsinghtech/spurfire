use std::{env, fs, net::SocketAddr};

use spurfire_net::{rustscale::RustScalePeer, PeerPayload, SessionState};
use spurfire_protocol::{LobbyId, PlayerId};
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
    let value = fs::read_to_string(path).map_err(|error| format!("read key file: {error}"))?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err("auth key file was empty".into());
    }
    Ok(Zeroizing::new(value))
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
    let mut session_a = SessionState::new(lobby, player_a, player_a, 0);
    let mut session_b = SessionState::new(lobby, player_b, player_a, 0);
    session_a.add_peer(player_b, 0);
    session_b.add_peer(player_a, 0);

    let destination_b = SocketAddr::new(b.tailnet_ip(), b.local_addr().port());
    let hello = session_a.envelope(
        1,
        PeerPayload::Hello {
            hostname: "spurfire-p2p-a".into(),
        },
    );
    a.send(&hello, destination_b).await?;
    let (received, source_a) = b.recv(Duration::from_secs(15)).await?;
    if session_b.accept(&received, 1) != spurfire_net::AcceptOutcome::Accepted {
        return Err("peer B rejected peer A hello".into());
    }

    let reply = session_b.envelope(
        2,
        PeerPayload::RiderInput {
            throttle_milli: 1_000,
            steer_milli: 250,
            buttons: 1,
        },
    );
    b.send(&reply, source_a).await?;
    let (received_reply, _) = a.recv(Duration::from_secs(15)).await?;
    if session_a.accept(&received_reply, 2) != spurfire_net::AcceptOutcome::Accepted {
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
