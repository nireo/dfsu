use std::{
    path::{Path, PathBuf},
    str,
};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey,
    endpoint::{Connection, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_tickets::endpoint::EndpointTicket;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    identity,
    manifest::{Manifest, checked_target_path},
};

const DFSU_SYNC_ALPN: &[u8] = b"/dfsu/sync/0";
const STREAM_CHUNK_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 1024;

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

        if let Err(err) = write_response_for_request(&self.root, &request, &mut send).await {
            let response = format!("error\t{err}\n");
            send.write_all(response.as_bytes())
                .await
                .map_err(AcceptError::from_err)?;
        }

        send.finish()?;
        connection.closed().await;

        Ok(())
    }
}

async fn write_response_for_request(
    root: &Path,
    request: &[u8],
    send: &mut SendStream,
) -> Result<()> {
    match request {
        b"manifest" => {
            let manifest = Manifest::from_scan(root)?.to_wire()?;
            send.write_all(manifest.as_bytes()).await?;
            Ok(())
        }
        request if request.starts_with(b"get\t") => {
            let name = str::from_utf8(&request[4..])?;
            stream_file_response(root, name, send).await
        }
        _ => bail!("unknown request"),
    }
}

async fn stream_file_response(root: &Path, name: &str, send: &mut SendStream) -> Result<()> {
    let path = readable_file_path(root, name)?;
    let size = std::fs::metadata(&path)?.len();
    let mut file = tokio::fs::File::open(path).await?;

    send.write_all(format!("file\t{size}\n").as_bytes()).await?;

    let mut buf = vec![0; STREAM_CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
    }

    Ok(())
}

fn readable_file_path(root: &Path, name: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = checked_target_path(&root, name)?;
    let path = path.canonicalize()?;

    anyhow::ensure!(path.starts_with(&root), "file escapes sync root");
    anyhow::ensure!(path.is_file(), "not a file: {name}");

    Ok(path)
}

fn writable_file_path(root: &Path, name: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let target = checked_target_path(&root, name)?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
        let parent = parent.canonicalize()?;
        anyhow::ensure!(parent.starts_with(&root), "target parent escapes sync root");
    }

    if target.exists() {
        let target = target.canonicalize()?;
        anyhow::ensure!(target.starts_with(&root), "target file escapes sync root");
    }

    Ok(target)
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
    println!("try: cargo run -- pair local {invite}");
    println!("then: cargo run -- sync {} local", path.display());

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

pub async fn download_remote_file(
    invite: &str,
    name: &str,
    root: &Path,
    expected_hash: &str,
) -> Result<()> {
    let endpoint = local_endpoint(IdentityMode::Ephemeral).await?;
    let addr = parse_endpoint_invite(invite)?;
    let connection = endpoint.connect(addr, DFSU_SYNC_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    send.write_all(format!("get\t{name}").as_bytes()).await?;
    send.finish()?;

    let target = writable_file_path(root, name)?;
    receive_file_to_path(&mut recv, &target, name, expected_hash).await?;
    endpoint.close().await;

    Ok(())
}

#[cfg(test)]
pub async fn request_remote_file(invite: &str, name: &str) -> Result<Vec<u8>> {
    let endpoint = local_endpoint(IdentityMode::Ephemeral).await?;
    let addr = parse_endpoint_invite(invite)?;
    let connection = endpoint.connect(addr, DFSU_SYNC_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    send.write_all(format!("get\t{name}").as_bytes()).await?;
    send.finish()?;

    let (size, initial_payload) = read_file_response_header(&mut recv).await?;
    let size = usize::try_from(size).context("file too large to buffer in memory")?;
    let mut out = Vec::with_capacity(size);
    append_response_bytes(&mut out, initial_payload, size)?;

    while out.len() < size {
        let chunk = recv
            .read_chunk(STREAM_CHUNK_BYTES)
            .await?
            .context("file response ended early")?;
        append_response_bytes(&mut out, chunk.bytes, size)?;
    }

    ensure_response_finished(&mut recv).await?;
    endpoint.close().await;

    Ok(out)
}

async fn receive_file_to_path(
    recv: &mut RecvStream,
    target: &Path,
    name: &str,
    expected_hash: &str,
) -> Result<()> {
    let parent = target.parent().context("target path has no parent")?;
    let temp = tempfile::Builder::new()
        .prefix(".dfsu-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    let temp_path = temp.into_temp_path();
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut received = 0_u64;

    let (size, initial_payload) = read_file_response_header(recv).await?;
    write_payload_chunk(
        &mut file,
        &mut hasher,
        &mut received,
        size,
        &initial_payload,
        name,
    )
    .await?;

    while received < size {
        let chunk = recv
            .read_chunk(STREAM_CHUNK_BYTES)
            .await?
            .context("file response ended early")?;
        write_payload_chunk(
            &mut file,
            &mut hasher,
            &mut received,
            size,
            &chunk.bytes,
            name,
        )
        .await?;
    }

    ensure_response_finished(recv).await?;
    file.flush().await?;
    drop(file);

    let actual_hash = hasher.finalize().to_hex().to_string();
    anyhow::ensure!(
        actual_hash == expected_hash,
        "hash mismatch for {name}: expected {expected_hash}, got {actual_hash}"
    );

    temp_path.persist(target).map_err(|err| err.error)?;

    Ok(())
}

async fn read_file_response_header(recv: &mut RecvStream) -> Result<(u64, Vec<u8>)> {
    let mut buf = Vec::new();

    loop {
        let chunk = recv
            .read_chunk(STREAM_CHUNK_BYTES)
            .await?
            .context("missing file response header")?;
        buf.extend_from_slice(&chunk.bytes);

        if let Some(header_end) = buf.iter().position(|byte| *byte == b'\n') {
            let payload = buf.split_off(header_end + 1);
            buf.truncate(header_end);
            let header = str::from_utf8(&buf)?;

            if let Some(error) = header.strip_prefix("error\t") {
                bail!("peer error: {}", error.trim());
            }

            let Some(size) = header.strip_prefix("file\t") else {
                bail!("invalid file response header");
            };
            let size = size.parse::<u64>()?;

            return Ok((size, payload));
        }

        anyhow::ensure!(
            buf.len() <= MAX_RESPONSE_HEADER_BYTES,
            "file response header is too large"
        );
    }
}

async fn write_payload_chunk(
    file: &mut tokio::fs::File,
    hasher: &mut blake3::Hasher,
    received: &mut u64,
    size: u64,
    bytes: &[u8],
    name: &str,
) -> Result<()> {
    let len = u64::try_from(bytes.len()).context("payload chunk is too large")?;
    anyhow::ensure!(
        *received + len <= size,
        "file response size mismatch for {name}"
    );

    file.write_all(bytes).await?;
    hasher.update(bytes);
    *received += len;

    Ok(())
}

#[cfg(test)]
fn append_response_bytes(out: &mut Vec<u8>, bytes: impl AsRef<[u8]>, size: usize) -> Result<()> {
    let bytes = bytes.as_ref();
    anyhow::ensure!(
        out.len() + bytes.len() <= size,
        "file response size mismatch"
    );
    out.extend_from_slice(bytes);

    Ok(())
}

async fn ensure_response_finished(recv: &mut RecvStream) -> Result<()> {
    if recv.read_chunk(1).await?.is_some() {
        bail!("file response size mismatch");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    async fn start_local_sync_server(root: &Path) -> (String, Router) {
        let endpoint = local_endpoint(IdentityMode::Ephemeral).await.unwrap();
        let invite = endpoint_invite(endpoint.addr());
        let router = Router::builder(endpoint)
            .accept(
                DFSU_SYNC_ALPN,
                LocalSyncProtocol {
                    root: root.to_path_buf(),
                },
            )
            .spawn();

        (invite, router)
    }

    #[derive(Debug, Clone)]
    struct StaticResponseProtocol {
        response: Vec<u8>,
    }

    impl ProtocolHandler for StaticResponseProtocol {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let _request = recv
                .read_to_end(1024)
                .await
                .map_err(AcceptError::from_err)?;

            send.write_all(&self.response)
                .await
                .map_err(AcceptError::from_err)?;
            send.finish()?;
            connection.closed().await;

            Ok(())
        }
    }

    async fn start_static_response_server(response: impl Into<Vec<u8>>) -> (String, Router) {
        let endpoint = local_endpoint(IdentityMode::Ephemeral).await.unwrap();
        let invite = endpoint_invite(endpoint.addr());
        let router = Router::builder(endpoint)
            .accept(
                DFSU_SYNC_ALPN,
                StaticResponseProtocol {
                    response: response.into(),
                },
            )
            .spawn();

        (invite, router)
    }

    fn hash(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    fn assert_no_temp_files(dir: &Path) {
        let temps = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".dfsu-") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();

        assert!(temps.is_empty(), "leftover temp files: {temps:?}");
    }

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
        let (invite, router) = start_local_sync_server(dir.path()).await;

        let manifest = request_remote_manifest(&invite).await.unwrap();

        assert!(manifest.files.contains_key("a.txt"));
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_sync_protocol_returns_file_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let (invite, router) = start_local_sync_server(dir.path()).await;

        let bytes = request_remote_file(&invite, "a.txt").await.unwrap();

        assert_eq!(bytes, b"hello");
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn downloads_file_by_streaming_to_target_path() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let bytes = vec![42_u8; STREAM_CHUNK_BYTES * 2 + 17];
        std::fs::write(source.path().join("large.bin"), &bytes).unwrap();
        let (invite, router) = start_local_sync_server(source.path()).await;

        download_remote_file(&invite, "large.bin", target.path(), &hash(&bytes))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(target.path().join("large.bin")).unwrap(),
            bytes
        );
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn downloads_nested_file_and_creates_parent_directories() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let nested = source.path().join("nested/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let bytes = vec![7_u8; STREAM_CHUNK_BYTES + 11];
        std::fs::write(nested.join("file.bin"), &bytes).unwrap();
        let (invite, router) = start_local_sync_server(source.path()).await;

        download_remote_file(
            &invite,
            "nested/deep/file.bin",
            target.path(),
            &hash(&bytes),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read(target.path().join("nested/deep/file.bin")).unwrap(),
            bytes
        );
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_hash_mismatch_without_persisting_new_file() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("a.txt"), b"remote contents").unwrap();
        let (invite, router) = start_local_sync_server(source.path()).await;

        let err = download_remote_file(&invite, "a.txt", target.path(), "not-the-right-hash")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("hash mismatch"));
        assert!(!target.path().join("a.txt").exists());
        assert_no_temp_files(target.path());
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn preserves_existing_target_when_hash_verification_fails() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("a.txt"), b"remote contents").unwrap();
        std::fs::write(target.path().join("a.txt"), b"local contents").unwrap();
        let (invite, router) = start_local_sync_server(source.path()).await;

        let err = download_remote_file(&invite, "a.txt", target.path(), "bad-hash")
            .await
            .unwrap_err();

        assert!(err.to_string().contains("hash mismatch"));
        assert_eq!(
            std::fs::read(target.path().join("a.txt")).unwrap(),
            b"local contents"
        );
        assert_no_temp_files(target.path());
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_truncated_file_response_without_persisting_file() {
        let target = tempfile::tempdir().unwrap();
        let (invite, router) = start_static_response_server(b"file\t5\nabc".to_vec()).await;

        let err = download_remote_file(&invite, "a.txt", target.path(), &hash(b"abc"))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("ended early"));
        assert!(!target.path().join("a.txt").exists());
        assert_no_temp_files(target.path());
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rejects_overlong_file_response_without_persisting_file() {
        let target = tempfile::tempdir().unwrap();
        let (invite, router) = start_static_response_server(b"file\t3\nabcd".to_vec()).await;

        let err = download_remote_file(&invite, "a.txt", target.path(), &hash(b"abc"))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("size mismatch"));
        assert!(!target.path().join("a.txt").exists());
        assert_no_temp_files(target.path());
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn propagates_peer_error_without_persisting_file() {
        let target = tempfile::tempdir().unwrap();
        let (invite, router) =
            start_static_response_server(b"error\tmissing file\n".to_vec()).await;

        let err = download_remote_file(&invite, "a.txt", target.path(), &hash(b""))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("peer error: missing file"));
        assert!(!target.path().join("a.txt").exists());
        assert_no_temp_files(target.path());
        router.shutdown().await.unwrap();
    }
}
