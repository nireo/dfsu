use std::{fs, path::PathBuf, str::FromStr};

use anyhow::{Context, Result};
use iroh_tickets::endpoint::EndpointTicket;

use crate::identity;

const PEERS_DIR: &str = "peers";

pub struct PeerStore {
    peers_dir: PathBuf,
}

impl PeerStore {
    pub fn open() -> Result<Self> {
        Ok(Self::new(identity::config_dir()?.join(PEERS_DIR)))
    }

    fn new(peers_dir: PathBuf) -> Self {
        Self { peers_dir }
    }

    pub fn save(&self, name: &str, invite: &str) -> Result<()> {
        validate_peer_name(name)?;
        validate_endpoint_invite(invite)?;
        fs::create_dir_all(&self.peers_dir)?;
        fs::write(self.peer_path(name)?, format!("{}\n", invite.trim()))?;

        Ok(())
    }

    pub fn resolve(&self, peer_or_invite: &str) -> Result<String> {
        if is_endpoint_invite(peer_or_invite) {
            return Ok(peer_or_invite.to_string());
        }

        self.load(peer_or_invite)
    }

    fn load(&self, name: &str) -> Result<String> {
        validate_peer_name(name)?;

        let invite = fs::read_to_string(self.peer_path(name)?)
            .with_context(|| format!("unknown peer {name:?}"))?;
        let invite = invite.trim().to_string();
        validate_endpoint_invite(&invite)?;

        Ok(invite)
    }

    fn peer_path(&self, name: &str) -> Result<PathBuf> {
        validate_peer_name(name)?;

        Ok(self.peers_dir.join(name))
    }
}

fn validate_peer_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.is_empty(), "peer name must not be empty");
    anyhow::ensure!(name != "." && name != "..", "invalid peer name");
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "peer name may only contain letters, numbers, '-' and '_'"
    );

    Ok(())
}

fn validate_endpoint_invite(invite: &str) -> Result<()> {
    EndpointTicket::from_str(invite.trim())?;

    Ok(())
}

fn is_endpoint_invite(value: &str) -> bool {
    EndpointTicket::from_str(value.trim()).is_ok()
}

#[cfg(test)]
mod tests {
    use iroh::EndpointAddr;

    use super::*;

    fn invite() -> String {
        EndpointTicket::new(EndpointAddr::new(iroh::SecretKey::generate().public())).to_string()
    }

    #[test]
    fn saves_and_loads_peer() {
        let dir = tempfile::tempdir().unwrap();
        let store = PeerStore::new(dir.path().to_path_buf());
        let invite = invite();

        store.save("laptop", &invite).unwrap();

        assert_eq!(store.resolve("laptop").unwrap(), invite);
    }

    #[test]
    fn resolves_raw_invites() {
        let store = PeerStore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let invite = invite();

        assert_eq!(store.resolve(&invite).unwrap(), invite);
    }

    #[test]
    fn rejects_unsafe_peer_names() {
        let store = PeerStore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let err = store.save("../x", "bad").unwrap_err();

        assert!(err.to_string().contains("peer name"));
    }

    #[test]
    fn rejects_invalid_invites() {
        let store = PeerStore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let err = store.save("laptop", "bad").unwrap_err();

        assert!(err.to_string().contains("wrong prefix"));
    }
}
