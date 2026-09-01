//! Where Copycat keeps its config, its database, and its socket.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const APP_DIR: &str = "copycat";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub database: PathBuf,
    pub key_file: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    /// Standard locations, overridable so tests and multiple profiles do not
    /// collide with a running daemon.
    pub fn resolve(
        config_override: Option<PathBuf>,
        data_override: Option<PathBuf>,
        socket_override: Option<PathBuf>,
    ) -> Result<Self> {
        let data_dir = match data_override {
            Some(dir) => dir,
            None => dirs::data_dir()
                .context("no data directory for this user")?
                .join(APP_DIR),
        };
        let config_file = match config_override {
            Some(file) => file,
            None => dirs::config_dir()
                .context("no config directory for this user")?
                .join(APP_DIR)
                .join("config.toml"),
        };
        let socket = match socket_override {
            Some(path) => path,
            None => default_socket(&data_dir),
        };

        Ok(Paths {
            database: data_dir.join("history.sqlite3"),
            key_file: data_dir.join("payload.key"),
            data_dir,
            config_file,
            socket,
        })
    }

    /// Create the data directory, and on Unix make sure it is not readable by
    /// anyone else — the key file and the socket both live under it (§23.1).
    pub fn prepare(&self) -> Result<()> {
        create_private_dir(&self.data_dir)?;
        if let Some(parent) = self.socket.parent() {
            create_private_dir(parent)?;
        }
        if let Some(parent) = self.config_file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn default_socket(data_dir: &Path) -> PathBuf {
    // A runtime directory is the right home for a socket: it is on tmpfs and
    // is cleared on logout, so a stale socket cannot outlive the session.
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(APP_DIR).join("daemon.sock"),
        _ => data_dir.join("daemon.sock"),
    }
}

#[cfg(windows)]
fn default_socket(_data_dir: &Path) -> PathBuf {
    // Interpreted as a named-pipe name rather than a filesystem path.
    PathBuf::from(format!(
        "copycat-{}",
        std::env::var("USERNAME").unwrap_or_else(|_| "user".into())
    ))
}

/// `0700` on Unix; the default ACL elsewhere.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir)?.permissions();
        if perms.mode() & 0o077 != 0 {
            perms.set_mode(0o700);
            std::fs::set_permissions(dir, perms)
                .with_context(|| format!("restricting {}", dir.display()))?;
        }
    }
    Ok(())
}

/// Refuse to use a directory other users can read or write.
///
/// The daemon serves decrypted history through the socket under here, so a
/// group-writable parent is not a warning — it is a reason not to start
/// (§23.1).
#[cfg(unix)]
pub fn verify_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "{} is mode {mode:04o}; it must not be accessible to other users",
            dir.display()
        );
    }
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid {
        anyhow::bail!(
            "{} is owned by uid {} but the daemon runs as uid {uid}",
            dir.display(),
            meta.uid()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn verify_private_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_win_and_derive_the_rest() {
        let paths = Paths::resolve(
            Some(PathBuf::from("/tmp/cfg.toml")),
            Some(PathBuf::from("/tmp/data")),
            Some(PathBuf::from("/tmp/sock")),
        )
        .unwrap();
        assert_eq!(paths.database, PathBuf::from("/tmp/data/history.sqlite3"));
        assert_eq!(paths.key_file, PathBuf::from("/tmp/data/payload.key"));
        assert_eq!(paths.config_file, PathBuf::from("/tmp/cfg.toml"));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(verify_private_dir(dir.path()).is_err());

        create_private_dir(dir.path()).unwrap();
        assert!(verify_private_dir(dir.path()).is_ok());
    }
}
