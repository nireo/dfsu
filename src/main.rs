use std::path::PathBuf;

use anyhow::Result;

use crate::{cli::Command, manifest::Manifest};

mod cli;
mod identity;
mod manifest;
mod net;
mod peers;

async fn init(path: PathBuf) -> Result<()> {
    let secret_key = identity::load_or_create_secret_key()?;

    println!("init {}", path.display());
    println!("identity {}", secret_key.public());
    Ok(())
}

async fn serve(path: PathBuf) -> Result<()> {
    net::serve_local(path).await
}

async fn pair(name: String, invite: String) -> Result<()> {
    let peers = peers::PeerStore::open()?;
    peers.save(&name, &invite)?;

    println!("paired {name}");
    Ok(())
}

async fn sync(path: PathBuf, peer: String) -> Result<()> {
    let peers = peers::PeerStore::open()?;
    let invite = peers.resolve(&peer)?;
    std::fs::create_dir_all(&path)?;
    let local_manifest = Manifest::from_scan(&path)?;
    let remote_manifest = net::request_remote_manifest(&invite).await?;
    let plan = local_manifest.plan_pull(&remote_manifest);

    println!("sync {}", path.display());
    println!("remote files: {}", remote_manifest.files.len());
    println!("files to download: {}", plan.download.len());

    for name in plan.download {
        let entry = &remote_manifest.files[&name];
        net::download_remote_file(&invite, &name, &path, &entry.hash).await?;
        println!("downloaded {name}");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    match cli::parse_command() {
        Command::Init { path } => init(path).await,
        Command::Serve { path } => serve(path).await,
        Command::Pair { name, invite } => pair(name, invite).await,
        Command::Sync { path, peer } => sync(path, peer).await,
    }
}
