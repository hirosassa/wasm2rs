//! Configuration of a [`WasiHost`]: [`WasiHostBuilder`] and the types it takes.

use super::{OpenDescriptor, Preopen, WasiHost};
use std::fmt;
use std::path::PathBuf;

/// What the guest may do under a preopened directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read files and directories only; every change is `read-only`.
    ReadOnly,
    /// Also create, write, truncate, rename and remove.
    ReadWrite,
}

/// Which network addresses the guest may use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// No socket address is allowed (`access-denied`); only `localhost` and numeric
    /// addresses resolve, locally.
    None,
    /// 127.0.0.0/8 and ::1 only; other host names are `permanent-resolver-failure`.
    #[default]
    Loopback,
    /// Any unicast address; host names are resolved by `std`'s resolver.
    External,
}

/// A configuration that cannot be built into a host.
#[derive(Debug)]
pub enum ConfigError {
    /// The preopen's host directory could not be resolved (usually: it does not exist).
    Preopen {
        host_path: PathBuf,
        source: std::io::Error,
    },
    /// The preopen's host path exists but is not a directory.
    NotADirectory(PathBuf),
    /// Two preopens have this guest name.
    DuplicatePreopen(String),
    /// A preopen's guest name is empty.
    EmptyPreopenName,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Preopen { host_path, source } => {
                write!(f, "preopen {}: {source}", host_path.display())
            }
            ConfigError::NotADirectory(path) => {
                write!(f, "preopen {}: not a directory", path.display())
            }
            ConfigError::DuplicatePreopen(name) => {
                write!(f, "two preopens are named {name:?}")
            }
            ConfigError::EmptyPreopenName => f.write_str("a preopen has an empty guest name"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Preopen { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Builds a [`WasiHost`]; see [`WasiHost::builder`].
#[derive(Debug, Default)]
pub struct WasiHostBuilder {
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<Option<String>>,
    preopens: Vec<(String, PathBuf, Access)>,
    network: NetworkPolicy,
}

impl WasiHostBuilder {
    /// The guest's command-line arguments (none by default).
    pub fn arguments(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// The guest's environment variables, replacing the process's (empty by default).
    pub fn environment(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// The guest's initial working directory. Without this call it is "/" if a preopen is
    /// named "/", else none; `None` here forces none.
    pub fn initial_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Shows the host directory `host_path` to the guest as `guest_name`. Descriptors
    /// 0, 1, ... are the preopens in the order of these calls.
    pub fn preopen(
        mut self,
        guest_name: impl Into<String>,
        host_path: impl Into<PathBuf>,
        access: Access,
    ) -> Self {
        self.preopens
            .push((guest_name.into(), host_path.into(), access));
        self
    }

    /// Which addresses the guest may use (loopback only by default).
    pub fn network(mut self, policy: NetworkPolicy) -> Self {
        self.network = policy;
        self
    }

    /// The host, or why the configuration is not valid.
    pub fn build(self) -> Result<WasiHost, ConfigError> {
        let mut host = WasiHost::new(self.env).with_arguments(self.args);
        host.cwd = self.cwd;
        host.network = self.network;
        for (guest_name, host_path, access) in self.preopens {
            host.add_preopen(guest_name, host_path, access)?;
        }
        Ok(host)
    }
}

impl WasiHost {
    /// A builder for a host with the guest's arguments, environment, preopens and network
    /// policy.
    pub fn builder() -> WasiHostBuilder {
        WasiHostBuilder::default()
    }

    /// Adds a preopen as the next descriptor slot.
    pub(super) fn add_preopen(
        &mut self,
        guest_name: String,
        host_path: PathBuf,
        access: Access,
    ) -> Result<(), ConfigError> {
        if guest_name.is_empty() {
            return Err(ConfigError::EmptyPreopenName);
        }
        if self.preopens.iter().any(|p| p.guest_name == guest_name) {
            return Err(ConfigError::DuplicatePreopen(guest_name));
        }
        let root = host_path
            .canonicalize()
            .map_err(|source| ConfigError::Preopen {
                host_path: host_path.clone(),
                source,
            })?;
        if !root.is_dir() {
            return Err(ConfigError::NotADirectory(host_path));
        }
        let slot = self.descriptors.insert(OpenDescriptor {
            path: root.clone(),
            preopen: self.preopens.len(),
            write: false,
            read: true,
            file: None,
        });
        debug_assert_eq!(slot, self.preopens.len(), "preopens are the first slots");
        self.preopens.push(Preopen {
            guest_name,
            root,
            writable: access == Access::ReadWrite,
        });
        Ok(())
    }
}
