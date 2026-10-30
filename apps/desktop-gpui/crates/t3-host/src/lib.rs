//! Launches and supervises the bundled Node server, the way the Electron shell
//! did in `apps/desktop/src/backend`. The server is unchanged: it receives the
//! same `DesktopBackendBootstrap` envelope (`packages/contracts/src/desktopBootstrap.ts`).

pub mod preferences;
pub mod window_state;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use t3_client::bootstrap_token;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use url::Url;

/// The desktop's preferred port; occupied ports scan upward from here.
pub const DEFAULT_PORT: u16 = 3773;
const READINESS_PATH: &str = "/.well-known/t3/environment";
const OUTPUT_TAIL_LINES: usize = 50;

/// Env vars that would make the child ignore the bootstrap envelope.
const SCRUBBED_ENV: &[&str] = &[
    "T3CODE_PORT",
    "T3CODE_MODE",
    "T3CODE_NO_BROWSER",
    "T3CODE_HOST",
    "T3CODE_DESKTOP_WS_URL",
    "T3CODE_DESKTOP_LAN_ACCESS",
    "T3CODE_DESKTOP_LAN_HOST",
    "T3CODE_DESKTOP_HTTPS_ENDPOINTS",
    "T3CODE_TAILSCALE_SERVE",
    "T3CODE_TAILSCALE_SERVE_PORT",
];

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("no free port at or above {0}")]
    NoFreePort(u16),
    #[error("could not start the server with {node}: {source}")]
    Spawn {
        node: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not deliver the bootstrap envelope: {0}")]
    Bootstrap(#[source] std::io::Error),
    #[error("the server exited during startup ({status})\n{output}")]
    ExitedDuringStartup { status: ExitStatus, output: String },
    #[error("the server was not ready after {}s\n{output}", .timeout.as_secs())]
    NotReady { timeout: Duration, output: String },
}

#[derive(Debug, Clone)]
pub struct BackendLaunch {
    /// A Node runtime satisfying `apps/server/package.json` `engines.node`.
    pub node: PathBuf,
    /// `apps/server/src/bin.ts` in development, the built `bin.mjs` when packaged.
    pub entry: PathBuf,
    pub cwd: PathBuf,
    /// Base data directory. Callers choose it explicitly so a development
    /// build can never land on the user's real `~/.t3`.
    pub t3_home: PathBuf,
    pub port: Option<u16>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapEnvelope<'a> {
    mode: &'static str,
    no_browser: bool,
    port: u16,
    t3_home: &'a str,
    host: &'static str,
    desktop_bootstrap_token: String,
    desktop_bootstrap_secret: &'a str,
    tailscale_serve_enabled: bool,
    tailscale_serve_port: u16,
}

pub struct Backend {
    child: Child,
    http_base: Url,
    secret: String,
    output: Arc<Mutex<VecDeque<String>>>,
}

impl Backend {
    pub async fn spawn(launch: BackendLaunch) -> Result<Self, HostError> {
        let port = match launch.port {
            Some(port) => port,
            None => find_free_port(DEFAULT_PORT)?,
        };
        let secret = random_secret();
        let t3_home = launch.t3_home.to_string_lossy();
        let envelope = BootstrapEnvelope {
            mode: "desktop",
            no_browser: true,
            port,
            t3_home: &t3_home,
            host: "127.0.0.1",
            desktop_bootstrap_token: bootstrap_token::current_token(&secret, now_ms()),
            desktop_bootstrap_secret: &secret,
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
        };
        let mut envelope = serde_json::to_vec(&envelope).expect("the envelope always serializes");
        envelope.push(b'\n');

        let mut command = Command::new(&launch.node);
        command
            .arg(&launch.entry)
            // stdin instead of fd 3 works the same on every platform; the
            // Electron shell already uses it for WSL.
            .args(["--bootstrap-fd", "0"])
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in SCRUBBED_ENV {
            command.env_remove(name);
        }
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe. The signal fires when the
        // spawning thread exits, which for a tokio worker is runtime shutdown.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }

        let mut child = command.spawn().map_err(|source| HostError::Spawn {
            node: launch.node.clone(),
            source,
        })?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin
            .write_all(&envelope)
            .await
            .map_err(HostError::Bootstrap)?;
        drop(stdin);

        let output = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stdout) = child.stdout.take() {
            forward_output(stdout, "stdout", output.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            forward_output(stderr, "stderr", output.clone());
        }

        let http_base = Url::parse(&format!("http://127.0.0.1:{port}")).expect("valid URL");
        tracing::info!(pid = child.id(), %http_base, "started server");
        Ok(Self {
            child,
            http_base,
            secret,
            output,
        })
    }

    pub fn http_base(&self) -> &Url {
        &self.http_base
    }

    /// The bootstrap credential valid right now. It rotates, so read it just
    /// before each exchange instead of caching it.
    pub fn bootstrap_token(&self) -> String {
        bootstrap_token::current_token(&self.secret, now_ms())
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Waits until the server answers its readiness probe.
    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<(), HostError> {
        let http = reqwest::Client::new();
        let mut url = self.http_base.clone();
        url.set_path(READINESS_PATH);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(HostError::ExitedDuringStartup {
                    status,
                    output: self.recent_output(),
                });
            }
            let probe = http.get(url.clone()).timeout(Duration::from_secs(2)).send();
            if let Ok(response) = probe.await
                && response.status().is_success()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(HostError::NotReady {
                    timeout,
                    output: self.recent_output(),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The exit status if the server has already stopped.
    pub fn try_exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// The last lines the server printed, for error reports.
    pub fn recent_output(&self) -> String {
        let lines = self.output.lock().expect("output lock");
        lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Asks the server to stop, then kills it after `grace`. A busy server
    /// needs a few seconds to stop its provider sessions; killing it outright
    /// leaves agents running.
    pub async fn shutdown(mut self, grace: Duration) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // SAFETY: plain syscall on a pid we spawned and have not reaped.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(grace, self.child.wait()).await.is_ok() {
                return;
            }
            tracing::warn!(pid, "server ignored SIGTERM; killing it");
        }
        #[cfg(not(unix))]
        let _ = grace;
        let _ = self.child.kill().await;
    }
}

fn forward_output(
    stream: impl AsyncRead + Unpin + Send + 'static,
    name: &'static str,
    tail: Arc<Mutex<VecDeque<String>>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(target: "t3_backend", stream = name, "{line}");
            let mut tail = tail.lock().expect("output lock");
            if tail.len() == OUTPUT_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    });
}

/// Binding and releasing races with other processes, as the Electron shell's
/// scan did; the server reports a clear error if it loses the race.
pub fn find_free_port(start: u16) -> Result<u16, HostError> {
    (start..=u16::MAX)
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .ok_or(HostError::NoFreePort(start))
}

fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS provides randomness");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
