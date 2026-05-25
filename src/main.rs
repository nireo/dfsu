use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};

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

async fn init(path: PathBuf) -> Result<()> {
    println!("init {}", path.display());
    Ok(())
}

async fn serve(path: PathBuf) -> Result<()> {
    // 1. Load persistent secret key.
    // 2. Scan `path` into a Manifest.
    // 3. Import files into an iroh-blobs store.
    // 4. Start an iroh Router with both dfsu-sync and iroh-blobs ALPNs.
    // 5. Answer manifest requests from paired peers.
    println!("serve {}", path.display());
    Ok(())
}

async fn pair(invite: String) -> Result<()> {
    // MVP: parse a one-time invite and store peer node id + last known address.
    println!("pair {invite}");
    Ok(())
}

async fn sync(path: PathBuf, peer: String) -> Result<()> {
    // 1. Load peer address from config.
    // 2. Scan local folder into a Manifest.
    // 3. Connect to peer's dfsu-sync protocol.
    // 4. Request remote Manifest.
    // 5. Run `plan_pull`.
    // 6. Download missing hashes through iroh-blobs.
    // 7. Write files using `safe_relative_path`.
    println!("sync {} from {peer}", path.display());
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
    fn rejects_path_traversal() {
        let err = safe_relative_path("notes/../secret.txt").unwrap_err();

        assert!(err.to_string().contains("unsafe path component"));
    }
}
