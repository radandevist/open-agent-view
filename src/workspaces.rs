use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const REGISTRY_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceRecord {
    pub path: PathBuf,
    pub last_used_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct RegistryDocument {
    version: u32,
    workspaces: Vec<WorkspaceRecord>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceRegistry {
    path: PathBuf,
    records: Arc<Mutex<Vec<WorkspaceRecord>>>,
}

impl WorkspaceRegistry {
    pub fn load_default() -> Result<Self> {
        Self::load(default_workspaces_path()?)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let parent = path
            .parent()
            .context("workspace registry path has no parent")?;
        ensure_private_directory(parent)?;
        let records = read_registry(&path)?;
        Ok(Self {
            path,
            records: Arc::new(Mutex::new(records)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Vec<WorkspaceRecord> {
        self.records
            .lock()
            .expect("workspace registry mutex poisoned")
            .clone()
    }

    pub fn validate_selection(&self, path: &Path) -> Result<PathBuf> {
        validate_workspace_selection(path)
    }

    pub fn record_launch(&self, path: &Path) -> Result<PathBuf> {
        self.record_launch_at(path, now_millis())
    }

    pub fn record_launch_at(&self, path: &Path, last_used_ms: u64) -> Result<PathBuf> {
        let canonical = self.validate_selection(path)?;
        let parent = self
            .path
            .parent()
            .context("workspace registry path has no parent")?;
        let _lock = RegistryLock::acquire(&parent.join("workspaces.lock"))?;
        let mut records = read_registry(&self.path)?;
        records.retain(|record| record.path != canonical);
        records.push(WorkspaceRecord {
            path: canonical.clone(),
            last_used_ms,
        });
        sort_records(&mut records);
        write_registry(&self.path, &records)?;
        *self
            .records
            .lock()
            .expect("workspace registry mutex poisoned") = records;
        Ok(canonical)
    }
}

pub fn default_workspaces_path() -> Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home)
            .join("open-agent-view")
            .join("workspaces.json"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/open-agent-view/workspaces.json"))
}

pub fn validate_workspace_selection(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("workspace must be an absolute path");
    }
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("workspace does not exist: {}", path.display()))?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("workspace must be an existing directory: {}", path.display());
    }
    Ok(canonical)
}

fn read_registry(path: &Path) -> Result<Vec<WorkspaceRecord>> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect workspace registry {}", path.display()))
        }
    }
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read workspace registry {}", path.display()))?;
    let document: RegistryDocument = serde_json::from_str(&input)
        .with_context(|| format!("invalid workspace registry {}", path.display()))?;
    if document.version != REGISTRY_VERSION {
        bail!(
            "unsupported workspace registry version {} in {}",
            document.version,
            path.display()
        );
    }
    let mut records = BTreeMap::new();
    for record in document.workspaces {
        let canonical = validate_workspace_selection(&record.path)
            .with_context(|| format!("invalid workspace {}", record.path.display()))?;
        if canonical != record.path {
            bail!("workspace registry contains a non-canonical path {}", record.path.display());
        }
        if records
            .insert(record.path.clone(), WorkspaceRecord { path: canonical, ..record })
            .is_some()
        {
            bail!("duplicate workspace path in {}", path.display());
        }
    }
    let mut records = records.into_values().collect::<Vec<_>>();
    sort_records(&mut records);
    Ok(records)
}

fn sort_records(records: &mut [WorkspaceRecord]) {
    records.sort_by(|left, right| {
        right
            .last_used_ms
            .cmp(&left.last_used_ms)
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn write_registry(path: &Path, records: &[WorkspaceRecord]) -> Result<()> {
    let parent = path
        .parent()
        .context("workspace registry path has no parent")?;
    ensure_private_directory(parent)?;
    reject_symlink(path)?;
    let temporary = path.with_file_name(format!(
        ".workspaces.tmp-{}-{}",
        std::process::id(),
        TEMPORARY_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let document = RegistryDocument {
        version: REGISTRY_VERSION,
        workspaces: records.to_vec(),
    };
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, &document)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::fs_util::replace_file(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    if !path.exists() {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create private directory {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{} must be a real directory", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} is not owned by the current user", path.display());
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            bail!("{} must have mode 0700", path.display());
        }
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("{} is not owned by the current user", path.display());
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            bail!("{} must have mode 0600", path.display());
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlinked workspace state path {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct RegistryLock {
    file: File,
}

impl RegistryLock {
    fn acquire(path: &Path) -> Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(_) => ensure_private_file(path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error()).context("failed to lock workspaces");
            }
        }
        Ok(Self { file })
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

static TEMPORARY_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::test_support::tempfile;

    #[test]
    fn persists_canonical_workspaces_newest_first_with_private_state() {
        let state = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let first = workspace_root.path().join("first");
        let second = workspace_root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let registry = WorkspaceRegistry::load(state.path().join("workspaces.json")).unwrap();

        registry.record_launch_at(&first.join("."), 10).unwrap();
        registry.record_launch_at(&second, 20).unwrap();
        registry.record_launch_at(&first, 20).unwrap();

        let paths = registry
            .list()
            .into_iter()
            .map(|record| record.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![
            fs::canonicalize(&first).unwrap(),
            fs::canonicalize(&second).unwrap(),
        ]);
        assert_eq!(fs::metadata(registry.path()).unwrap().permissions().readonly(), false);
        let restored = WorkspaceRegistry::load(registry.path().to_owned()).unwrap();
        assert_eq!(restored.list().len(), 2);
    }

    #[test]
    fn rejects_relative_missing_duplicate_and_malformed_entries() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("workspaces.json");
        fs::write(
            &path,
            r#"{"version":1,"workspaces":[{"path":"relative","last_used_ms":1}]}"#,
        )
        .unwrap();
        assert!(WorkspaceRegistry::load(path).is_err());

        let absolute = PathBuf::from("/definitely/not/a/real/workspace");
        assert!(validate_workspace_selection(&absolute).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_or_public_state() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let state = tempfile::tempdir().unwrap();
        let target = state.path().join("target.json");
        fs::write(&target, r#"{"version":1,"workspaces":[]}"#).unwrap();
        let link = state.path().join("workspaces.json");
        symlink(&target, &link).unwrap();
        assert!(WorkspaceRegistry::load(link).is_err());

        let parent = state.path().join("public");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(WorkspaceRegistry::load(parent.join("workspaces.json")).is_err());
    }
}
