use std::{
    collections::BTreeMap,
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration as StdDuration, Instant},
};

use serde::{Deserialize, Serialize};
use spurfire_net::{
    rustscale::{RustScalePeer, RustScaleTransportError},
    AcceptOutcome, PeerPayload, SessionState,
};
use spurfire_protocol::{LobbyId, PlayerId};
use tokio::time::Duration;
use zeroize::Zeroizing;

const LOBBY: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_A: &str = "00000000-0000-4000-8000-000000000001";
const PLAYER_B: &str = "00000000-0000-4000-8000-000000000002";
const PLAYER_C: &str = "00000000-0000-4000-8000-000000000003";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MeshConfig {
    endpoints: BTreeMap<String, SocketAddr>,
}

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
        thread::sleep(StdDuration::from_millis(50));
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
    wait_for(&config_path, StdDuration::from_secs(30))?;
    let config: MeshConfig = serde_json::from_slice(&fs::read(config_path)?)?;
    let lobby = LobbyId::parse(LOBBY)?;
    let local = player(&node)?;
    let authority = PlayerId::parse(PLAYER_A)?;
    let mut session = SessionState::new(lobby, local, authority, 0);
    for name in ["a", "b", "c"] {
        session.add_peer(player(name)?, 0);
    }

    let started = Instant::now();
    let mut last_send = StdDuration::ZERO;
    let mut saw_authority = node == "a";
    let mut saw_other_survivor = node == "a";
    let mut migration_announced = false;
    let mut gameplay_continued = false;

    loop {
        if directory.join("stop").exists() {
            break;
        }
        let elapsed = started.elapsed();
        let now_ms = elapsed.as_millis().try_into().unwrap_or(u64::MAX);
        let send_interval = if node == "c"
            && session.authority() == PlayerId::parse(PLAYER_B)?
            && session.authority_epoch() >= 2
        {
            StdDuration::from_millis(50)
        } else {
            StdDuration::from_millis(200)
        };
        if elapsed.saturating_sub(last_send) >= send_interval {
            last_send = elapsed;
            let payload =
                if node == "b" && session.authority() == local && session.authority_epoch() >= 2 {
                    PeerPayload::Authority {
                        authority: local,
                        epoch: session.authority_epoch(),
                    }
                } else if node == "c"
                    && session.authority() == PlayerId::parse(PLAYER_B)?
                    && session.authority_epoch() >= 2
                {
                    PeerPayload::RiderInput {
                        throttle_milli: 1_000,
                        steer_milli: 200,
                        buttons: 1,
                    }
                } else {
                    PeerPayload::Heartbeat
                };
            let envelope = session.envelope(now_ms / 16, payload);
            for (name, destination) in &config.endpoints {
                if name != &node {
                    let _ = peer.send(&envelope, *destination).await;
                }
            }
        }

        match peer.recv(Duration::from_millis(40)).await {
            Ok((envelope, _)) => {
                let sender = envelope.sender;
                let payload = envelope.payload.clone();
                if session.accept(&envelope, now_ms) == AcceptOutcome::Accepted {
                    if sender == PlayerId::parse(PLAYER_A)? {
                        saw_authority = true;
                    }
                    if (node == "b" && sender == PlayerId::parse(PLAYER_C)?)
                        || (node == "c" && sender == PlayerId::parse(PLAYER_B)?)
                    {
                        saw_other_survivor = true;
                    }
                    if node == "b"
                        && session.authority_epoch() >= 2
                        && matches!(payload, PeerPayload::RiderInput { .. })
                    {
                        gameplay_continued = true;
                        write_atomic(&directory.join("continued-b"), b"ok")?;
                    }
                    if node == "c"
                        && session.authority() == PlayerId::parse(PLAYER_B)?
                        && session.authority_epoch() >= 2
                    {
                        gameplay_continued = true;
                        write_atomic(&directory.join("continued-c"), b"ok")?;
                    }
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
            if session.authority() == PlayerId::parse(PLAYER_B)? && session.authority_epoch() >= 2 {
                migration_announced = true;
                write_atomic(&directory.join(format!("migrated-{node}")), b"ok")?;
            }
        }
        if elapsed > StdDuration::from_secs(45) && (!migration_announced || !gameplay_continued) {
            return Err(
                format!("node {node} did not complete migration and continued play").into(),
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
        let mut endpoints = BTreeMap::new();
        for node in ["a", "b", "c"] {
            let path = directory.join(format!("ready-{node}"));
            wait_for(&path, StdDuration::from_secs(45))?;
            endpoints.insert(node.to_owned(), fs::read_to_string(path)?.parse()?);
        }
        write_atomic(
            &directory.join("mesh.json"),
            &serde_json::to_vec(&MeshConfig { endpoints })?,
        )?;
        wait_for(&directory.join("mesh-b"), StdDuration::from_secs(15))?;
        wait_for(&directory.join("mesh-c"), StdDuration::from_secs(15))?;

        // Simulate abrupt authority process loss: no Leave packet and no graceful close.
        a.kill()?;
        let _ = a.wait();
        wait_for(&directory.join("migrated-b"), StdDuration::from_secs(12))?;
        wait_for(&directory.join("migrated-c"), StdDuration::from_secs(12))?;
        wait_for(&directory.join("continued-b"), StdDuration::from_secs(20))?;
        wait_for(&directory.join("continued-c"), StdDuration::from_secs(20))?;
        write_atomic(&directory.join("stop"), b"ok")?;
        if !b.wait()?.success() || !c.wait()?.success() {
            return Err("surviving peer process failed".into());
        }
        println!("SPURFIRE_MIGRATION_OK authority=a successor=b epoch=2 continued_play=true");
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
