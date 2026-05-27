use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{Connection, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_tickets::endpoint::EndpointTicket;
use walkdir::WalkDir;

const DFSU_SYNC_ALPN: &[u8] = b"/dfsu/sync/0";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Init { path: PathBuf },
    Serve { path: PathBuf },
    Pair { invite: String },
    Sync { path: PathBuf, peer: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    hash: String,
    size: u64,
    modified_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Manifest {
    folder_id: String,
    files: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PullPlan {
    download: Vec<String>,
}

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

impl Manifest {
    fn to_wire(&self) -> Result<String> {
        let mut out = String::new();

        ensure_wire_field(&self.folder_id)?;
        out.push_str("folder\t");
        out.push_str(&self.folder_id);
        out.push('\n');

        for (path, entry) in &self.files {
            ensure_wire_field(path)?;
            ensure_wire_field(&entry.hash)?;
            out.push_str("file\t");
            out.push_str(&entry.hash);
            out.push('\t');
            out.push_str(&entry.size.to_string());
            out.push('\t');
            out.push_str(&entry.modified_ms.to_string());
            out.push('\t');
            out.push_str(path);
            out.push('\n');
        }

        Ok(out)
    }

    fn from_wire(input: &str) -> Result<Self> {
        let mut lines = input.lines();
        let header = lines.next().context("missing manifest header")?;
        let Some(folder_id) = header.strip_prefix("folder\t") else {
            bail!("invalid manifest header");
        };

        let mut manifest = Manifest {
            folder_id: folder_id.to_string(),
            files: BTreeMap::new(),
        };

        for line in lines {
            let parts = line.splitn(5, '\t').collect::<Vec<_>>();
            match parts.as_slice() {
                ["file", hash, size, modified_ms, path] => {
                    safe_relative_path(path)?;
                    manifest.files.insert(
                        (*path).to_string(),
                        ManifestEntry {
                            hash: (*hash).to_string(),
                            size: size.parse().context("invalid file size")?,
                            modified_ms: modified_ms.parse().context("invalid modified time")?,
                        },
                    );
                }
                _ => bail!("invalid manifest line: {line:?}"),
            }
        }

        Ok(manifest)
    }

    fn plan_pull(&self, remote: &Manifest) -> PullPlan {
        let mut download = Vec::new();

        for (path, remote_entry) in &remote.files {
            match self.files.get(path) {
                Some(local_entry) if local_entry.hash == remote_entry.hash => {}
                _ => download.push(path.clone()),
            }
        }

        PullPlan { download }
    }

    fn from_scan(root: &Path) -> Result<Manifest> {
        let root = root.canonicalize()?;
        anyhow::ensure!(
            root.is_dir(),
            "sync root must be a directory: {}",
            root.display()
        );

        let mut manifest = Manifest {
            folder_id: root.display().to_string(),
            files: BTreeMap::new(),
        };

        for entry in WalkDir::new(&root) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let relative = path.strip_prefix(&root)?;
            let name = relative_path_name(relative)?;
            let metadata = entry.metadata()?;
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default();

            manifest.files.insert(
                name,
                ManifestEntry {
                    hash: hash_file(path)?,
                    size: metadata.len(),
                    modified_ms,
                },
            );
        }

        Ok(manifest)
    }
}

fn parse_command(args: &[String]) -> Result<Command> {
    match args {
        [cmd, path] if cmd == "init" => Ok(Command::Init {
            path: PathBuf::from(path),
        }),
        [cmd, path] if cmd == "serve" => Ok(Command::Serve {
            path: PathBuf::from(path),
        }),
        [cmd, invite] if cmd == "pair" => Ok(Command::Pair {
            invite: invite.clone(),
        }),
        [cmd, path, peer] if cmd == "sync" => Ok(Command::Sync {
            path: PathBuf::from(path),
            peer: peer.clone(),
        }),
        _ => bail!(
            "usage: dfsu init <path> | dfsu serve <path> | dfsu pair <invite> | dfsu sync <path> <peer>"
        ),
    }
}

fn ensure_wire_field(value: &str) -> Result<()> {
    if value.contains('\t') || value.contains('\n') || value.contains('\r') {
        bail!("manifest fields must not contain tabs or newlines");
    }

    Ok(())
}

fn relative_path_name(path: &Path) -> Result<String> {
    let mut parts = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().context("path is not valid utf-8")?;
                if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                    bail!("unsafe path component: {part:?}");
                }
                ensure_wire_field(part)?;
                parts.push(part);
            }
            _ => bail!("unsafe path component: {component:?}"),
        }
    }

    anyhow::ensure!(!parts.is_empty(), "relative path must not be empty");

    Ok(parts.join("/"))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0; 8 * 1024];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

fn safe_relative_path(name: &str) -> Result<PathBuf> {
    anyhow::ensure!(!name.is_empty(), "relative path must not be empty");

    let mut path = PathBuf::new();

    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            bail!("unsafe path component: {part:?}");
        }

        path.push(part);
    }

    Ok(path)
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

async fn request_remote_manifest(invite: &str) -> Result<Manifest> {
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

async fn init(path: PathBuf) -> Result<()> {
    println!("init {}", path.display());
    Ok(())
}

async fn serve(path: PathBuf) -> Result<()> {
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

async fn pair(invite: String) -> Result<()> {
    // MVP: parse a one-time invite and store peer node id + last known address.
    println!("pair {invite}");
    Ok(())
}

async fn sync(path: PathBuf, peer: String) -> Result<()> {
    let local_manifest = Manifest::from_scan(&path)?;
    let remote_manifest = request_remote_manifest(&peer).await?;
    let plan = local_manifest.plan_pull(&remote_manifest);

    println!("sync {}", path.display());
    println!("remote files: {}", remote_manifest.files.len());
    println!("files to download: {}", plan.download.len());

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    match parse_command(&args)? {
        Command::Init { path } => init(path).await,
        Command::Serve { path } => serve(path).await,
        Command::Pair { invite } => pair(invite).await,
        Command::Sync { path, peer } => sync(path, peer).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn entry(hash: &str) -> ManifestEntry {
        ManifestEntry {
            hash: hash.to_string(),
            size: 1,
            modified_ms: 1,
        }
    }

    #[test]
    fn parses_sync_command() {
        let command = parse_command(&args(&["sync", "./Sync", "laptop"])).unwrap();

        assert_eq!(
            command,
            Command::Sync {
                path: PathBuf::from("./Sync"),
                peer: "laptop".to_string(),
            }
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let err = parse_command(&args(&["pull", "./Sync", "laptop"])).unwrap_err();

        assert!(err.to_string().contains("usage:"));
    }

    #[test]
    fn pull_plan_downloads_missing_files() {
        let local = Manifest::default();
        let mut remote = Manifest::default();
        remote.files.insert("notes/todo.md".to_string(), entry("a"));

        let plan = local.plan_pull(&remote);

        assert_eq!(plan.download, vec!["notes/todo.md"]);
    }

    #[test]
    fn pull_plan_skips_matching_files() {
        let mut local = Manifest::default();
        let mut remote = Manifest::default();
        local.files.insert("notes/todo.md".to_string(), entry("a"));
        remote.files.insert("notes/todo.md".to_string(), entry("a"));

        let plan = local.plan_pull(&remote);

        assert!(plan.download.is_empty());
    }

    #[test]
    fn pull_plan_downloads_changed_files() {
        let mut local = Manifest::default();
        let mut remote = Manifest::default();
        local.files.insert("notes/todo.md".to_string(), entry("a"));
        remote.files.insert("notes/todo.md".to_string(), entry("b"));

        let plan = local.plan_pull(&remote);

        assert_eq!(plan.download, vec!["notes/todo.md"]);
    }

    #[test]
    fn manifest_wire_round_trips() {
        let mut manifest = Manifest {
            folder_id: "/tmp/example".to_string(),
            files: BTreeMap::new(),
        };
        manifest
            .files
            .insert("notes/todo.md".to_string(), entry("a"));

        let wire = manifest.to_wire().unwrap();
        let parsed = Manifest::from_wire(&wire).unwrap();

        assert_eq!(parsed, manifest);
    }

    #[test]
    fn scans_folder_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("todo.md"), b"hello").unwrap();

        let manifest = Manifest::from_scan(dir.path()).unwrap();
        let file = manifest.files.get("notes/todo.md").unwrap();

        assert_eq!(file.hash, blake3::hash(b"hello").to_hex().to_string());
        assert_eq!(file.size, 5);
    }

    #[test]
    fn accepts_safe_relative_paths() {
        let path = safe_relative_path("notes/todo.md").unwrap();

        assert_eq!(path, PathBuf::from("notes").join("todo.md"));
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

    #[test]
    fn rejects_path_traversal() {
        let err = safe_relative_path("notes/../secret.txt").unwrap_err();

        assert!(err.to_string().contains("unsafe path component"));
    }
}
