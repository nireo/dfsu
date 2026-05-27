use std::path::PathBuf;

use anyhow::{Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{Connection, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_tickets::endpoint::EndpointTicket;

use crate::manifest::Manifest;

const DFSU_SYNC_ALPN: &[u8] = b"/dfsu/sync/0";

#[derive(Debug, Clone)]
struct LocalSyncProtocol {
    root: PathBuf,
}

impl ProtocolHandler for LocalSyncProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let request = recv
            .read_to_end(1024)
            .await
            .map_err(AcceptError::from_err)?;

        let response = match request.as_slice() {
            b"manifest" => Manifest::from_scan(&self.root)
                .and_then(|manifest| manifest.to_wire())
                .unwrap_or_else(|err| format!("error\t{err}\n")),
            _ => "error\tunknown request\n".to_string(),
        };

        send.write_all(response.as_bytes())
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;

        Ok(())
    }
}

async fn local_endpoint() -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .clear_address_lookup()
        .clear_ip_transports()
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")?
        .bind()
        .await?;

    Ok(endpoint)
}

fn endpoint_invite(addr: EndpointAddr) -> String {
    EndpointTicket::new(addr).to_string()
}

fn parse_endpoint_invite(invite: &str) -> Result<EndpointAddr> {
    let ticket: EndpointTicket = invite.parse()?;

    Ok(ticket.endpoint_addr().clone())
}

pub async fn serve_local(path: PathBuf) -> Result<()> {
    let endpoint = local_endpoint().await?;
    let invite = endpoint_invite(endpoint.addr());
    let router = Router::builder(endpoint)
        .accept(DFSU_SYNC_ALPN, LocalSyncProtocol { root: path.clone() })
        .spawn();

    println!("serving {}", path.display());
    println!("local invite: {invite}");
    println!("try: cargo run -- sync {} {invite}", path.display());

    tokio::signal::ctrl_c().await?;
    router.shutdown().await?;

    Ok(())
}

pub async fn request_remote_manifest(invite: &str) -> Result<Manifest> {
    let endpoint = local_endpoint().await?;
    let addr = parse_endpoint_invite(invite)?;
    let connection = endpoint.connect(addr, DFSU_SYNC_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    send.write_all(b"manifest").await?;
    send.finish()?;

    let response = recv.read_to_end(1024 * 1024).await?;
    endpoint.close().await;
    let response = String::from_utf8(response)?;
    if let Some(err) = response.strip_prefix("error\t") {
        bail!("peer error: {}", err.trim());
    }

    Manifest::from_wire(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_invites_round_trip() {
        let addr = EndpointAddr::new(iroh::SecretKey::generate().public());
        let invite = endpoint_invite(addr.clone());

        assert_eq!(parse_endpoint_invite(&invite).unwrap(), addr);
    }

    #[tokio::test]
    async fn local_sync_protocol_returns_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();

        let endpoint = local_endpoint().await.unwrap();
        let invite = endpoint_invite(endpoint.addr());
        let router = Router::builder(endpoint)
            .accept(
                DFSU_SYNC_ALPN,
                LocalSyncProtocol {
                    root: dir.path().to_path_buf(),
                },
            )
            .spawn();

        let manifest = request_remote_manifest(&invite).await.unwrap();

        assert!(manifest.files.contains_key("a.txt"));
        router.shutdown().await.unwrap();
    }
}
