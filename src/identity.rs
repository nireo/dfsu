use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use iroh::SecretKey;

const SECRET_FILE: &str = "secret.key";

pub fn load_or_create_secret_key() -> Result<SecretKey> {
    load_or_create_secret_key_at(config_dir()?.join(SECRET_FILE))
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DFSU_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }

    let home = std::env::var_os("HOME").context("HOME is not set")?;

    Ok(PathBuf::from(home).join(".config").join("dfsu"))
}

fn load_or_create_secret_key_at(path: PathBuf) -> Result<SecretKey> {
    match fs::read_to_string(&path) {
        Ok(secret) => SecretKey::from_str(secret.trim()).context("invalid secret key"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            write_secret_key(&path, &key)?;
            Ok(key)
        }
        Err(err) => Err(err).context("failed to read secret key"),
    }
}

fn write_secret_key(path: &Path, key: &SecretKey) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, hex::encode(key.to_bytes()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SECRET_FILE);

        let first = load_or_create_secret_key_at(path.clone()).unwrap();
        let second = load_or_create_secret_key_at(path).unwrap();

        assert_eq!(first.public(), second.public());
    }
}
