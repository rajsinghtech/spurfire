//! RustScale tsnet UDP transport pinned by the crate manifest.

use std::net::{IpAddr, SocketAddr};

use rustscale_netstack::UdpListener;
use rustscale_tsnet::{Server, TsnetError};
use tempfile::TempDir;
use thiserror::Error;
use tokio::time::{timeout, Duration};
use zeroize::Zeroizing;

use spurfire_protocol::NodeKey;

use crate::{decode, encode, CodecError, Envelope, MAX_DATAGRAM_BYTES};

#[derive(Debug, Error)]
pub enum RustScaleTransportError {
    #[error(transparent)]
    RustScale(#[from] TsnetError),
    #[error("rustscale UDP transport failed: {0}")]
    Netstack(String),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("rustscale peer receive timed out")]
    Timeout,
    #[error("rustscale assigned no tailnet IP")]
    NoTailnetIp,
    #[error("failed to create private state directory: {0}")]
    StateDirectory(String),
}

/// RustScale server owner that clears builder-copied enrollment material on every exit path.
struct ClearedAuthServer(Server);

impl Drop for ClearedAuthServer {
    fn drop(&mut self) {
        self.0.clear_auth_key();
    }
}

/// One ephemeral embedded RustScale node with an application UDP socket.
pub struct RustScalePeer {
    server: ClearedAuthServer,
    socket: UdpListener,
    state_dir: TempDir,
    tailnet_ip: IpAddr,
}

impl std::fmt::Debug for RustScalePeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RustScalePeer")
            .field("tailnet_ip", &self.tailnet_ip)
            .field("local_addr", &self.socket.local_addr())
            .field("state_dir", &"<private temporary directory>")
            .finish()
    }
}

impl RustScalePeer {
    /// Enroll an ephemeral node, clear the one-use key, and bind application UDP.
    pub async fn connect(
        hostname: impl Into<String>,
        auth_key: Zeroizing<Vec<u8>>,
        port: u16,
    ) -> Result<Self, RustScaleTransportError> {
        let state_dir = tempfile::Builder::new()
            .prefix("spurfire-rustscale-")
            .tempdir()
            .map_err(|error| RustScaleTransportError::StateDirectory(error.to_string()))?;
        // Keep the controlled owner byte-oriented until the pinned RustScale
        // builder's final borrowed UTF-8 boundary. The builder may transiently
        // copy internally; that third-party copy cannot be claimed zeroized.
        let auth_key_text = std::str::from_utf8(&auth_key)
            .map_err(|error| RustScaleTransportError::Netstack(error.to_string()))?;
        let mut server = ClearedAuthServer(
            Server::builder()
                .hostname(hostname)
                .auth_key(auth_key_text)
                .state_dir(state_dir.path())
                .ephemeral(true)
                .build()?,
        );
        let status = server.0.up().await;
        server.0.clear_auth_key();
        drop(auth_key);
        let status = status?;
        let tailnet_ip = *status
            .tailscale_ips
            .first()
            .ok_or(RustScaleTransportError::NoTailnetIp)?;
        let socket = server.0.listen_packet(&format!(":{port}")).await?;
        Ok(Self {
            server,
            socket,
            state_dir,
            tailnet_ip,
        })
    }

    #[must_use]
    pub fn tailnet_ip(&self) -> IpAddr {
        self.tailnet_ip
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr()
    }

    #[must_use]
    pub fn private_state_path(&self) -> &std::path::Path {
        self.state_dir.path()
    }

    /// Current coarse route classification from RustScale magicsock.
    #[must_use]
    pub fn route_to(&self, peer_ip: IpAddr) -> Option<String> {
        self.server
            .0
            .status()
            .peers
            .into_iter()
            .find(|peer| peer.ips.contains(&peer_ip))
            .map(|peer| format!("{:?}", peer.path_class))
    }

    /// Maps a WireGuard-authenticated source IP to the current netmap node key.
    /// Rotation changes this value and therefore requires a fresh registration.
    #[must_use]
    pub fn node_key_for(&self, peer_ip: IpAddr) -> Option<NodeKey> {
        self.server
            .0
            .status()
            .peers
            .into_iter()
            .find(|peer| peer.ips.contains(&peer_ip))
            .and_then(|peer| NodeKey::parse(&peer.node_key.to_string()).ok())
    }

    pub async fn send(
        &self,
        envelope: &Envelope,
        destination: SocketAddr,
    ) -> Result<(), RustScaleTransportError> {
        let bytes = encode(envelope)?;
        self.send_datagram(&bytes, destination).await
    }

    /// Carries one already validated Spurfire wire datagram without imposing
    /// a wire-major interpretation at the transport layer.
    pub async fn send_datagram(
        &self,
        bytes: &[u8],
        destination: SocketAddr,
    ) -> Result<(), RustScaleTransportError> {
        if bytes.len() > MAX_DATAGRAM_BYTES {
            return Err(CodecError::TooLarge.into());
        }
        self.socket
            .send_to(bytes, destination)
            .await
            .map_err(|error| RustScaleTransportError::Netstack(error.to_string()))
    }

    pub async fn recv(
        &mut self,
        wait: Duration,
    ) -> Result<(Envelope, SocketAddr), RustScaleTransportError> {
        let (bytes, source) = self.recv_datagram(wait).await?;
        Ok((decode(&bytes)?, source))
    }

    /// Receives one bounded opaque datagram for the active application codec.
    pub async fn recv_datagram(
        &mut self,
        wait: Duration,
    ) -> Result<(Vec<u8>, SocketAddr), RustScaleTransportError> {
        let (bytes, source) = timeout(wait, self.socket.recv_from())
            .await
            .map_err(|_| RustScaleTransportError::Timeout)?
            .map_err(|error| RustScaleTransportError::Netstack(error.to_string()))?;
        if bytes.len() > MAX_DATAGRAM_BYTES {
            return Err(CodecError::TooLarge.into());
        }
        Ok((bytes.to_vec(), source))
    }

    /// Close the embedded node. RustScale may ask the caller to retry while
    /// platform port-mapper cleanup is still settling.
    pub async fn close(&mut self) -> Result<(), RustScaleTransportError> {
        self.server.0.close().await?;
        Ok(())
    }
}
