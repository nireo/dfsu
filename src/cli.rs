use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "dfsu", version, about = "Local-first folder sync over iroh")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Create or load a local identity for a sync folder.
    Init { path: PathBuf },

    /// Serve a folder and print a local invite.
    Serve { path: PathBuf },

    /// Save a peer invite under a friendly name.
    Pair { name: String, invite: String },

    /// Pull missing or changed files from a peer into a folder.
    Sync { path: PathBuf, peer: String },
}

pub fn parse_command() -> Command {
    Cli::parse().command
}

#[cfg(test)]
pub fn try_parse_command_from<I, T>(args: I) -> Result<Command, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Ok(Cli::try_parse_from(args)?.command)
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn parses_sync_command() {
        let command = try_parse_command_from(["dfsu", "sync", "./Sync", "laptop"]).unwrap();

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
        let command = try_parse_command_from(["dfsu", "pair", "laptop", "endpointabc"]).unwrap();

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
        let err = try_parse_command_from(["dfsu", "pull", "./Sync", "laptop"]).unwrap_err();

        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
    }
}
