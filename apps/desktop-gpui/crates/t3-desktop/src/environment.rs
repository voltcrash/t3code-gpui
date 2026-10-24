//! The local environment: the server this app launches, the socket to it, and
//! the shell state it streams. One owner keeps retries in one place, as the
//! connection supervisor does for the other clients
//! (`docs/internals/connection-runtime.md`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use futures_util::StreamExt;
use gpui_kit::{AsyncApp, Context, SharedString, Subscription, Task, WeakEntity};
use serde_json::{Value, json};
use t3_client::RpcClient;
use t3_client::auth::{self, ClientMetadata};
use t3_client::commands;
use t3_client::shell::{self, ShellCapabilities, ShellState, ShellStreamItem};
use t3_host::{Backend, BackendLaunch};
use url::Url;

use crate::tokio_bridge::{self, Tokio};

/// The first start runs every database migration.
const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// A busy server needs a few seconds to stop its provider sessions.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const RETRY_INITIAL: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub enum ConnectionState {
    StartingServer,
    Connecting,
    Connected,
    Reconnecting,
    Failed(SharedString),
}

pub struct LocalEnvironment {
    pub state: ConnectionState,
    pub shell: ShellState,
    pub capabilities: ShellCapabilities,
    /// The live socket, for views that open their own subscriptions.
    pub rpc: Option<RpcClient>,
    /// The server resolves send and stop commands (`commands::supported`).
    pub commands_supported: bool,
    _run: Task<()>,
    _quit: Subscription,
}

impl LocalEnvironment {
    pub fn start(launch: BackendLaunch, cx: &mut Context<Self>) -> Self {
        let backend = Arc::new(Mutex::new(None));
        let run = cx.spawn({
            let backend = backend.clone();
            async move |this, cx| {
                if let Err(error) = run(this.clone(), launch, backend, cx).await {
                    tracing::error!("{error:#}");
                    let _ = this.update(cx, |env, cx| {
                        env.state = ConnectionState::Failed(format!("{error:#}").into());
                        cx.notify();
                    });
                }
            }
        });
        // Quit hooks only get a few hundred milliseconds, so block here for
        // the graceful stop instead of leaving agents running.
        let quit = cx.on_app_quit({
            let backend = backend.clone();
            move |_, cx| {
                let backend = backend.lock().expect("backend lock").take();
                if let Some(backend) = backend {
                    cx.global::<Tokio>()
                        .0
                        .block_on(backend.shutdown(SHUTDOWN_GRACE));
                }
                async {}
            }
        });
        Self {
            state: ConnectionState::StartingServer,
            shell: ShellState::default(),
            capabilities: ShellCapabilities::default(),
            rpc: None,
            commands_supported: false,
            _run: run,
            _quit: quit,
        }
    }
}

async fn run(
    this: WeakEntity<LocalEnvironment>,
    launch: BackendLaunch,
    slot: Arc<Mutex<Option<Backend>>>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let started = cx.update(|cx| {
        tokio_bridge::spawn(cx, async move {
            let mut backend = Backend::spawn(launch).await?;
            backend.wait_ready(READY_TIMEOUT).await?;
            anyhow::Ok(backend)
        })
    });
    let backend = started.await??;
    let http_base = backend.http_base().clone();
    *slot.lock().expect("backend lock") = Some(backend);
    set_state(&this, cx, ConnectionState::Connecting)?;

    let http = reqwest::Client::new();
    let mut retry = RETRY_INITIAL;
    loop {
        let token = {
            let mut backend = slot.lock().expect("backend lock");
            let backend = backend
                .as_mut()
                .ok_or_else(|| anyhow!("the server was stopped"))?;
            if let Some(status) = backend.try_exited() {
                return Err(anyhow!(
                    "the server exited ({status})\n{}",
                    backend.recent_output()
                ));
            }
            backend.bootstrap_token()
        };
        let connecting = cx
            .update(|cx| tokio_bridge::spawn(cx, connect(http.clone(), http_base.clone(), token)));
        let connected = match connecting.await? {
            Ok(rpc) => rpc
                .call::<_, Value>("server.getConfig", &json!({}))
                .await
                .map(|config| (rpc, config))
                .map_err(anyhow::Error::from),
            Err(error) => Err(error),
        };
        let (rpc, config) = match connected {
            Ok(connected) => connected,
            Err(error) => {
                tracing::warn!("connecting failed: {error:#}");
                set_state(&this, cx, ConnectionState::Reconnecting)?;
                cx.background_executor().timer(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
                continue;
            }
        };
        retry = RETRY_INITIAL;

        let capabilities = &config["environment"]["capabilities"];
        let after_sequence = this.update(cx, |env, cx| {
            env.capabilities = ShellCapabilities {
                settlement: capabilities["threadSettlement"] == true,
                snooze: capabilities["threadSnooze"] == true,
            };
            env.state = ConnectionState::Connected;
            env.rpc = Some(rpc.clone());
            env.commands_supported = commands::supported(&config);
            cx.notify();
            env.shell.sequence
        })?;

        // Resuming after a reconnect replays only what was missed.
        let input = match after_sequence {
            Some(sequence) => json!({ "afterSequence": sequence }),
            None => json!({}),
        };
        let mut updates = rpc.subscribe::<_, ShellStreamItem>(shell::SUBSCRIBE, &input);
        while let Some(item) = updates.next().await {
            match item {
                Ok(item) => this.update(cx, |env, cx| {
                    if let ShellStreamItem::Snapshot { snapshot, .. } = &item {
                        tracing::info!(
                            threads = snapshot.threads.len(),
                            projects = snapshot.projects.len(),
                            "shell snapshot"
                        );
                    }
                    if env.shell.apply(item) {
                        cx.notify();
                    }
                })?,
                Err(error) => {
                    tracing::warn!("shell stream ended: {error}");
                    break;
                }
            }
        }
        set_state(&this, cx, ConnectionState::Reconnecting)?;
    }
}

const CLIENT_LABEL: &str = "T3 Code desktop";

async fn connect(
    http: reqwest::Client,
    http_base: Url,
    bootstrap_token: String,
) -> anyhow::Result<RpcClient> {
    let metadata = ClientMetadata {
        label: CLIENT_LABEL.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    let access =
        auth::exchange_bootstrap_credential(&http, &http_base, &bootstrap_token, &metadata).await?;
    let url = auth::websocket_url(&http, &http_base, &access.access_token, &metadata).await?;
    Ok(RpcClient::connect(&url).await?)
}

fn set_state(
    this: &WeakEntity<LocalEnvironment>,
    cx: &mut AsyncApp,
    state: ConnectionState,
) -> anyhow::Result<()> {
    this.update(cx, |env, cx| {
        if !matches!(state, ConnectionState::Connected) {
            env.rpc = None;
        }
        env.state = state;
        cx.notify();
    })
}
