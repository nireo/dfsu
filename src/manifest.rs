use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub hash: String,
    pub size: u64,
    pub modified_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub folder_id: String,
    pub files: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PullPlan {
    pub download: Vec<String>,
}

impl Manifest {
    pub fn to_wire(&self) -> Result<String> {
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

    pub fn from_wire(input: &str) -> Result<Self> {
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

    pub fn plan_pull(&self, remote: &Manifest) -> PullPlan {
        let mut download = Vec::new();

        for (path, remote_entry) in &remote.files {
            match self.files.get(path) {
                Some(local_entry) if local_entry.hash == remote_entry.hash => {}
                _ => download.push(path.clone()),
            }
        }

        PullPlan { download }
    }

    pub fn from_scan(root: &Path) -> Result<Manifest> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hash: &str) -> ManifestEntry {
        ManifestEntry {
            hash: hash.to_string(),
            size: 1,
            modified_ms: 1,
        }
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
    fn rejects_path_traversal() {
        let err = safe_relative_path("notes/../secret.txt").unwrap_err();

        assert!(err.to_string().contains("unsafe path component"));
    }
}
