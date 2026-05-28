use std::{
    path::{Path, PathBuf},
    str,
};

use anyhow::{Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey,
    endpoint::{Connection, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_tickets::endpoint::EndpointTicket;

use crate::{
    identity,
    manifest::{Manifest, checked_target_path},
};

const DFSU_SYNC_ALPN: &[u8] = b"/dfsu/sync/0";
const MAX_FILE_RESPONSE_BYTES: usize = 1024 * 1024 * 1024;

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

        let response = match response_for_request(&self.root, &request) {
            Ok(response) => response,
            Err(err) => format!("error\t{err}\n").into_bytes(),
        };

        send.write_all(&response)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;

        Ok(())
    }
}

fn response_for_request(root: &Path, request: &[u8]) -> Result<Vec<u8>> {
    match request {
        b"manifest" => Manifest::from_scan(root)
            .and_then(|manifest| manifest.to_wire())
            .map(String::into_bytes),
        request if request.starts_with(b"get\t") => {
            let name = str::from_utf8(&request[4..])?;
            let path = readable_file_path(root, name)?;
            let bytes = std::fs::read(path)?;
            let mut response = format!("file\t{}\n", bytes.len()).into_bytes();
            response.extend_from_slice(&bytes);
            Ok(response)
        }
        _ => bail!("unknown request"),
    }
}

fn readable_file_path(root: &Path, name: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = checked_target_path(&root, name)?;
    let path = path.canonicalize()?;

    anyhow::ensure!(path.starts_with(&root), "file escapes sync root");
    anyhow::ensure!(path.is_file(), "not a file: {name}");

    Ok(path)
}

enum IdentityMode {
    Persistent,
    Ephemeral,
}

async fn local_endpoint(identity_mode: IdentityMode) -> Result<Endpoint> {
    let secret_key = match identity_mode {
        IdentityMode::Persistent => identity::load_or_create_secret_key()?,
        IdentityMode::Ephemeral => SecretKey::generate(),
    };
    let endpoint = Endpoint::builder(presets::N0)
        .clear_address_lookup()
        .clear_ip_transports()
        .relay_mode(RelayMode::Disabled)
        .secret_key(secret_key)
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
    let endpoint = local_endpoint(IdentityMode::Persistent).await?;
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
    let endpoint = local_endpoint(IdentityMode::Ephemeral).await?;
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

pub async fn request_remote_file(invite: &str, name: &str) -> Result<Vec<u8>> {
    let endpoint = local_endpoint(IdentityMode::Ephemeral).await?;
    let addr = parse_endpoint_invite(invite)?;
    let connection = endpoint.connect(addr, DFSU_SYNC_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    send.write_all(format!("get\t{name}").as_bytes()).await?;
    send.finish()?;

    let response = recv.read_to_end(MAX_FILE_RESPONSE_BYTES).await?;
    endpoint.close().await;

    file_response_payload(&response)
}

fn file_response_payload(response: &[u8]) -> Result<Vec<u8>> {
    if response.starts_with(b"error\t") {
        let error = String::from_utf8_lossy(&response[6..]);
        bail!("peer error: {}", error.trim());
    }

    let Some(header_end) = response.iter().position(|byte| *byte == b'\n') else {
        bail!("missing file response header");
    };
    let header = str::from_utf8(&response[..header_end])?;
    let Some(size) = header.strip_prefix("file\t") else {
        bail!("invalid file response header");
    };
    let size = size.parse::<usize>()?;
    let payload = &response[header_end + 1..];

    anyhow::ensure!(payload.len() == size, "file response size mismatch");

    Ok(payload.to_vec())
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

        let endpoint = local_endpoint(IdentityMode::Ephemeral).await.unwrap();
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

    #[tokio::test]
    async fn local_sync_protocol_returns_file_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();

        let endpoint = local_endpoint(IdentityMode::Ephemeral).await.unwrap();
        let invite = endpoint_invite(endpoint.addr());
        let router = Router::builder(endpoint)
            .accept(
                DFSU_SYNC_ALPN,
                LocalSyncProtocol {
                    root: dir.path().to_path_buf(),
                },
            )
            .spawn();

        let bytes = request_remote_file(&invite, "a.txt").await.unwrap();

        assert_eq!(bytes, b"hello");
        router.shutdown().await.unwrap();
    }
}
