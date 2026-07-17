//! RustScale tsnet UDP transport pinned by the crate manifest.

use std::net::{IpAddr, SocketAddr};

use rustscale_netstack::UdpListener;
use rustscale_tsnet::{Server, TsnetError};
use tempfile::TempDir;
use thiserror::Error;
use tokio::time::{timeout, Duration};
use zeroize::Zeroizing;

use crate::{decode, encode, CodecError, Envelope};

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

/// One ephemeral embedded RustScale node with an application UDP socket.
pub struct RustScalePeer {
    server: Server,
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
        auth_key: Zeroizing<String>,
        port: u16,
    ) -> Result<Self, RustScaleTransportError> {
        let state_dir = tempfile::Builder::new()
            .prefix("spurfire-rustscale-")
            .tempdir()
            .map_err(|error| RustScaleTransportError::StateDirectory(error.to_string()))?;
        let mut server = Server::builder()
            .hostname(hostname)
            .auth_key(auth_key.as_str())
            .state_dir(state_dir.path())
            .ephemeral(true)
            .build()?;
        let status = server.up().await?;
        server.clear_auth_key();
        drop(auth_key);
        let tailnet_ip = *status
            .tailscale_ips
            .first()
            .ok_or(RustScaleTransportError::NoTailnetIp)?;
        let socket = server.listen_packet(&format!(":{port}")).await?;
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
            .status()
            .peers
            .into_iter()
            .find(|peer| peer.ips.contains(&peer_ip))
            .map(|peer| format!("{:?}", peer.path_class))
    }

    pub async fn send(
        &self,
        envelope: &Envelope,
        destination: SocketAddr,
    ) -> Result<(), RustScaleTransportError> {
        let bytes = encode(envelope)?;
        self.socket
            .send_to(&bytes, destination)
            .await
            .map_err(|error| RustScaleTransportError::Netstack(error.to_string()))
    }

    pub async fn recv(
        &mut self,
        wait: Duration,
    ) -> Result<(Envelope, SocketAddr), RustScaleTransportError> {
        let (bytes, source) = timeout(wait, self.socket.recv_from())
            .await
            .map_err(|_| RustScaleTransportError::Timeout)?
            .map_err(|error| RustScaleTransportError::Netstack(error.to_string()))?;
        Ok((decode(&bytes)?, source))
    }

    /// Close the embedded node. RustScale may ask the caller to retry while
    /// platform port-mapper cleanup is still settling.
    pub async fn close(&mut self) -> Result<(), RustScaleTransportError> {
        self.server.close().await?;
        Ok(())
    }
}
