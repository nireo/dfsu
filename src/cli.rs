use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init { path: PathBuf },
    Serve { path: PathBuf },
    Pair { name: String, invite: String },
    Sync { path: PathBuf, peer: String },
}

pub fn parse_command(args: &[String]) -> Result<Command> {
    match args {
        [cmd, path] if cmd == "init" => Ok(Command::Init {
            path: PathBuf::from(path),
        }),
        [cmd, path] if cmd == "serve" => Ok(Command::Serve {
            path: PathBuf::from(path),
        }),
        [cmd, name, invite] if cmd == "pair" => Ok(Command::Pair {
            name: name.clone(),
            invite: invite.clone(),
        }),
        [cmd, path, peer] if cmd == "sync" => Ok(Command::Sync {
            path: PathBuf::from(path),
            peer: peer.clone(),
        }),
        _ => bail!(
            "usage: dfsu init <path> | dfsu serve <path> | dfsu pair <name> <invite> | dfsu sync <path> <peer>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
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
    fn parses_pair_command() {
        let command = parse_command(&args(&["pair", "laptop", "endpointabc"])).unwrap();

        assert_eq!(
            command,
            Command::Pair {
                name: "laptop".to_string(),
                invite: "endpointabc".to_string(),
            }
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let err = parse_command(&args(&["pull", "./Sync", "laptop"])).unwrap_err();

        assert!(err.to_string().contains("usage:"));
    }
}
