use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{Connection, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_tickets::endpoint::EndpointTicket;

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
        let request = recv.read_to_end(1024).await.map_err(AcceptError::from_err)?;

        let response = match request.as_slice() {
            b"manifest" => format!("manifest {}\n", self.root.display()),
            _ => "error unknown-request\n".to_string(),
        };

        send.write_all(response.as_bytes())
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;

        Ok(())
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

fn plan_pull(local: &Manifest, remote: &Manifest) -> PullPlan {
    let mut download = Vec::new();

    for (path, remote_entry) in &remote.files {
        match local.files.get(path) {
            Some(local_entry) if local_entry.hash == remote_entry.hash => {}
            _ => download.push(path.clone()),
        }
    }

    PullPlan { download }
}

fn safe_relative_path(name: &str) -> Result<PathBuf> {
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

async fn request_remote_manifest(invite: &str) -> Result<String> {
    let endpoint = local_endpoint().await?;
    let addr = parse_endpoint_invite(invite)?;
    let connection = endpoint.connect(addr, DFSU_SYNC_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    send.write_all(b"manifest").await?;
    send.finish()?;

    let response = recv.read_to_end(1024 * 1024).await?;
    endpoint.close().await;

    Ok(String::from_utf8(response)?)
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
    let remote_manifest = request_remote_manifest(&peer).await?;

    println!("sync {}", path.display());
    println!("remote: {}", remote_manifest.trim());

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

        let plan = plan_pull(&local, &remote);

        assert_eq!(plan.download, vec!["notes/todo.md"]);
    }

    #[test]
    fn pull_plan_skips_matching_files() {
        let mut local = Manifest::default();
        let mut remote = Manifest::default();
        local.files.insert("notes/todo.md".to_string(), entry("a"));
        remote.files.insert("notes/todo.md".to_string(), entry("a"));

        let plan = plan_pull(&local, &remote);

        assert!(plan.download.is_empty());
    }

    #[test]
    fn pull_plan_downloads_changed_files() {
        let mut local = Manifest::default();
        let mut remote = Manifest::default();
        local.files.insert("notes/todo.md".to_string(), entry("a"));
        remote.files.insert("notes/todo.md".to_string(), entry("b"));

        let plan = plan_pull(&local, &remote);

        assert_eq!(plan.download, vec!["notes/todo.md"]);
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
    async fn local_sync_protocol_returns_manifest_stub() {
        let endpoint = local_endpoint().await.unwrap();
        let invite = endpoint_invite(endpoint.addr());
        let router = Router::builder(endpoint)
            .accept(
                DFSU_SYNC_ALPN,
                LocalSyncProtocol {
                    root: PathBuf::from("./Sync"),
                },
            )
            .spawn();

        let response = request_remote_manifest(&invite).await.unwrap();

        assert_eq!(response, "manifest ./Sync\n");
        router.shutdown().await.unwrap();
    }

    #[test]
    fn rejects_path_traversal() {
        let err = safe_relative_path("notes/../secret.txt").unwrap_err();

        assert!(err.to_string().contains("unsafe path component"));
    }
}
