//! Configuration of a [`WasiHost`]: [`WasiHostBuilder`] and the types it takes.

use super::{Clock, HttpClient, OpenDescriptor, Preopen, RandomSource, WasiHost};
use std::fmt;
use std::io::{Read, Write};
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
    /// No [`RandomSource`] was given and the `os-random` feature (the default one) is off.
    MissingRandomSource,
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
            ConfigError::MissingRandomSource => f.write_str(
                "no random source: give one to the builder or enable the `os-random` feature",
            ),
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
#[derive(Default)]
pub struct WasiHostBuilder {
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<Option<String>>,
    preopens: Vec<(String, PathBuf, Access)>,
    network: NetworkPolicy,
    clock: Option<Box<dyn Clock>>,
    random: Option<Box<dyn RandomSource>>,
    http_client: Option<Box<dyn HttpClient>>,
    max_request_body: Option<usize>,
    stdin: Option<Box<dyn Read + Send>>,
    stdout: Option<Box<dyn Write + Send>>,
    stderr: Option<Box<dyn Write + Send>>,
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
    /// Each preopen is confined and checked for access on its own: if one directory lies
    /// inside another, the guest reaches it through the outer preopen with the outer access.
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

    /// The time source (the system's by default). See [`Clock`] for what it does not affect.
    pub fn clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Some(Box::new(clock));
        self
    }

    /// The source of `wasi:random`. Without this call the OS's is used if the `os-random`
    /// feature is on; with the feature off, `build` fails.
    pub fn random(mut self, random: impl RandomSource + 'static) -> Self {
        self.random = Some(Box::new(random));
        self
    }

    /// The client behind `wasi:http/outgoing-handler`. Without one, a guest's requests fail with
    /// `internal-error`.
    pub fn http_client(mut self, client: impl HttpClient + 'static) -> Self {
        self.http_client = Some(Box::new(client));
        self
    }

    /// The most a request body may hold (1 MiB by default): the body is buffered until the
    /// guest finishes it. `check-write` of its stream shrinks to the room left, and a write
    /// past the limit fails the stream and the request with `HTTP-request-body-size`.
    pub fn max_request_body(mut self, bytes: usize) -> Self {
        self.max_request_body = Some(bytes);
        self
    }

    /// What the guest reads as stdin (the process's by default). `reader` is read on a thread
    /// that starts when the guest first uses stdin, and its end or failure reaches the guest
    /// as the stream's.
    pub fn stdin(mut self, reader: impl Read + Send + 'static) -> Self {
        self.stdin = Some(Box::new(reader));
        self
    }

    /// Where the guest's stdout goes (the process's by default). It is flushed when the guest
    /// flushes the stream, at `wasi:cli/exit` and when the host is dropped; wrap a buffered
    /// sink yourself if you want buffering.
    pub fn stdout(mut self, writer: impl Write + Send + 'static) -> Self {
        self.stdout = Some(Box::new(writer));
        self
    }

    /// Where the guest's stderr goes (the process's by default); flushed like stdout.
    pub fn stderr(mut self, writer: impl Write + Send + 'static) -> Self {
        self.stderr = Some(Box::new(writer));
        self
    }

    /// The host, or why the configuration is not valid.
    pub fn build(self) -> Result<WasiHost, ConfigError> {
        let mut host = WasiHost::new(self.env).with_arguments(self.args);
        host.cwd = self.cwd;
        host.network = self.network;
        host.stdin_reader = self.stdin;
        host.stdout = self.stdout;
        host.stderr = self.stderr;
        if let Some(clock) = self.clock {
            host.clock = clock;
        }
        if let Some(random) = self.random {
            host.random = Some(random);
        }
        host.http_client = self.http_client;
        if let Some(limit) = self.max_request_body {
            host.http_max_body = limit;
        }
        if host.random.is_none() {
            return Err(ConfigError::MissingRandomSource);
        }
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
