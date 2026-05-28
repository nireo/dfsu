use std::path::PathBuf;

use anyhow::Result;

use crate::{
    cli::Command,
    manifest::{Manifest, write_verified_file},
};

mod cli;
mod identity;
mod manifest;
mod net;

async fn init(path: PathBuf) -> Result<()> {
    let secret_key = identity::load_or_create_secret_key()?;

    println!("init {}", path.display());
    println!("identity {}", secret_key.public());
    Ok(())
}

async fn serve(path: PathBuf) -> Result<()> {
    net::serve_local(path).await
}

async fn pair(invite: String) -> Result<()> {
    // MVP: parse a one-time invite and store peer node id + last known address.
    println!("pair {invite}");
    Ok(())
}

async fn sync(path: PathBuf, peer: String) -> Result<()> {
    std::fs::create_dir_all(&path)?;
    let local_manifest = Manifest::from_scan(&path)?;
    let remote_manifest = net::request_remote_manifest(&peer).await?;
    let plan = local_manifest.plan_pull(&remote_manifest);

    println!("sync {}", path.display());
    println!("remote files: {}", remote_manifest.files.len());
    println!("files to download: {}", plan.download.len());

    for name in plan.download {
        let entry = &remote_manifest.files[&name];
        let bytes = net::request_remote_file(&peer, &name).await?;
        write_verified_file(&path, &name, &bytes, &entry.hash)?;
        println!("downloaded {name}");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();

    match cli::parse_command(&args)? {
        Command::Init { path } => init(path).await,
        Command::Serve { path } => serve(path).await,
        Command::Pair { invite } => pair(invite).await,
        Command::Sync { path, peer } => sync(path, peer).await,
    }
}
