//! Where Copycat keeps its config, its database, and its socket.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use copycat_protocol::APP_DIR;

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
            None => copycat_protocol::default_socket_path()
                .unwrap_or_else(|| data_dir.join("daemon.sock")),
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

/// Create a directory only Copycat's user can enter, and refuse to use one
/// that already exists and is open to others.
///
/// The directory is created `0700` in one step rather than created and then
/// tightened, so there is no window where the key file or socket inside it is
/// reachable. An existing directory is never silently re-permissioned: it may
/// be one the user chose, and quietly changing the mode of `~/sockets` because
/// Copycat was pointed at it would be worse than refusing.
pub fn create_private_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    verify_private_dir(dir)
}

/// Refuse to use a directory other users can reach.
///
/// The daemon serves decrypted history through the socket under here and keeps
/// the payload key beside it, so a shared parent is not a warning — it is a
/// reason not to start (§23.1).
#[cfg(unix)]
pub fn verify_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    let uid = unsafe { libc::getuid() };

    if meta.uid() != uid {
        anyhow::bail!(
            "{} is owned by uid {}, but the daemon runs as uid {uid}",
            dir.display(),
            meta.uid()
        );
    }

    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "{} is mode {mode:04o}, which lets other users in.\n\
             Copycat keeps its socket and payload key here, and the socket serves decrypted \
             clipboard history, so it will not start with a shared directory.\n\
             Use a directory only you can enter, such as $XDG_RUNTIME_DIR/copycat, or run \
             `chmod 700 {}`.",
            dir.display(),
            dir.display()
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
    fn a_shared_directory_is_refused_with_an_actionable_message() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = create_private_dir(dir.path()).unwrap_err().to_string();

        assert!(error.contains("0755"), "{error}");
        assert!(error.contains("chmod 700"), "the message should say how to fix it: {error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_created_directory_is_private_from_the_moment_it_exists() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("nested/run");

        create_private_dir(&dir).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
