use std::path::PathBuf;

use anyhow::Result;

use crate::{cli::Command, manifest::Manifest};

mod cli;
mod manifest;
mod net;

async fn init(path: PathBuf) -> Result<()> {
    println!("init {}", path.display());
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
    let local_manifest = Manifest::from_scan(&path)?;
    let remote_manifest = net::request_remote_manifest(&peer).await?;
    let plan = local_manifest.plan_pull(&remote_manifest);

    println!("sync {}", path.display());
    println!("remote files: {}", remote_manifest.files.len());
    println!("files to download: {}", plan.download.len());

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
