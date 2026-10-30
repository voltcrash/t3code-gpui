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
use t3_client::config::{self, ConfigStreamItem, ServerState};
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
    pub message_drafts: t3_client::message_drafts::Drafts,
    pub new_thread_drafts: t3_client::new_thread_drafts::Drafts,
    pub draft_storage_error: Option<SharedString>,
    draft_store: t3_host::message_drafts::Store,
    saved_drafts: Option<t3_host::message_drafts::SavedDrafts>,
    saved_draft_revision: Option<(u64, (u64, u64))>,
    notifications: t3_client::notifications::Tracker,
    pub capabilities: ShellCapabilities,
    /// The live socket, for views that open their own subscriptions.
    pub rpc: Option<RpcClient>,
    pub history: Option<t3_client::history::HistoryClient>,
    pub attachments: Option<t3_client::attachments::Client>,
    /// The server resolves send and stop commands (`commands::supported`).
    pub commands_supported: bool,
    pub thread_actions: t3_client::thread_actions::Capabilities,
    pub archives: t3_client::archives::State,
    pub archive_error: Option<SharedString>,
    archive_requested: bool,
    archive_task: Option<Task<()>>,
    /// Providers and settings, current while connected.
    pub server: ServerState,
    _run: Task<()>,
    _quit: Subscription,
}

impl LocalEnvironment {
    pub fn thread(&self, id: &str) -> Option<&t3_client::shell::ThreadShell> {
        self.shell
            .threads
            .get(id)
            .or_else(|| self.archives.threads.get(id))
            .or_else(|| self.shell.archived.get(id))
    }

    pub fn load_archives(&mut self, cx: &mut Context<Self>) {
        self.archive_requested = true;
        if self.archive_task.is_some() {
            return;
        }
        let Some(rpc) = self.rpc.clone() else {
            return;
        };
        self.archive_task = Some(cx.spawn(async move |this, cx| {
            let mut retry = RETRY_INITIAL;
            loop {
                let mut updates = rpc.subscribe::<_, t3_client::archives::StreamItem>(
                    t3_client::archives::SUBSCRIBE,
                    &json!({}),
                );
                while let Some(item) = updates.next().await {
                    match item {
                        Ok(item) => {
                            retry = RETRY_INITIAL;
                            if this
                                .update(cx, |this, cx| {
                                    this.archive_error = None;
                                    if this.archives.apply(item) {
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = this.update(cx, |this, cx| {
                                this.archive_error = Some(error.detail().into());
                                cx.notify();
                            });
                            break;
                        }
                    }
                }
                if rpc.is_closed() {
                    return;
                }
                cx.background_executor().timer(retry).await;
                retry = (retry * 2).min(RETRY_MAX);
            }
        }));
        cx.notify();
    }

    pub fn save_drafts(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.saved_drafts.as_mut() else {
            return;
        };
        let revision = (
            self.message_drafts.revision(),
            self.new_thread_drafts.revision(),
        );
        if self.saved_draft_revision == Some(revision) {
            return;
        }
        saved.threads = self.message_drafts.snapshot();
        saved.new_threads = self.new_thread_drafts.snapshot();
        match self.draft_store.save(saved) {
            Ok(()) => {
                self.saved_draft_revision = Some(revision);
                self.draft_storage_error = None;
            }
            Err(error) => {
                tracing::error!("saving message drafts failed: {error}");
                self.draft_storage_error =
                    Some("Couldn't save unsent messages on this device.".into());
            }
        }
        cx.notify();
    }

    pub fn retry_draft_storage(&mut self, cx: &mut Context<Self>) {
        if self.saved_drafts.is_none() {
            match self.draft_store.load() {
                Ok(saved) => {
                    self.message_drafts.restore_missing(&saved.threads);
                    self.new_thread_drafts.restore_missing(&saved.new_threads);
                    self.saved_drafts = Some(saved);
                }
                Err(error) => {
                    tracing::error!("loading message drafts failed: {error}");
                    return;
                }
            }
        }
        self.save_drafts(cx);
    }

    pub fn start(launch: BackendLaunch, cx: &mut Context<Self>) -> Self {
        let draft_store = t3_host::message_drafts::Store::new(&launch.t3_home);
        let mut message_drafts = t3_client::message_drafts::Drafts::default();
        let mut new_thread_drafts = t3_client::new_thread_drafts::Drafts::default();
        let (saved_drafts, saved_draft_revision, draft_storage_error) = match draft_store.load() {
            Ok(saved) => {
                message_drafts.restore_missing(&saved.threads);
                new_thread_drafts.restore_missing(&saved.new_threads);
                let revision = (message_drafts.revision(), new_thread_drafts.revision());
                (Some(saved), Some(revision), None)
            }
            Err(error) => {
                tracing::error!("loading message drafts failed: {error}");
                (
                    None,
                    None,
                    Some("Couldn't restore unsent messages on this device.".into()),
                )
            }
        };
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
            move |this, cx| {
                this.save_drafts(cx);
                let attachments = this.attachments.clone();
                let backend = backend.lock().expect("backend lock").take();
                if let Some(backend) = backend {
                    cx.global::<Tokio>().0.block_on(async move {
                        if let Some(attachments) = attachments {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(1),
                                attachments.discard_pending(),
                            )
                            .await;
                        }
                        backend.shutdown(SHUTDOWN_GRACE).await;
                    });
                }
                async {}
            }
        });
        Self {
            state: ConnectionState::StartingServer,
            shell: ShellState::default(),
            message_drafts,
            new_thread_drafts,
            draft_storage_error,
            draft_store,
            saved_drafts,
            saved_draft_revision,
            notifications: Default::default(),
            capabilities: ShellCapabilities::default(),
            rpc: None,
            history: None,
            attachments: None,
            commands_supported: false,
            thread_actions: Default::default(),
            archives: Default::default(),
            archive_error: None,
            archive_requested: false,
            archive_task: None,
            server: ServerState::default(),
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
            Ok((rpc, history, attachments)) => rpc
                .call::<_, Value>("server.getConfig", &json!({}))
                .await
                .map(|config| (rpc, history, attachments, config))
                .map_err(anyhow::Error::from),
            Err(error) => Err(error),
        };
        let (rpc, history, attachments, config) = match connected {
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
            env.history = Some(history);
            let attachments = env.attachments.clone().unwrap_or(attachments);
            attachments.reconnected(&rpc);
            env.attachments = Some(attachments);
            env.archive_task = None;
            if env.archive_requested {
                env.load_archives(cx);
            }
            env.commands_supported = commands::supported(&config);
            env.thread_actions = t3_client::thread_actions::Capabilities::from_config(&config);
            env.server = ServerState::default();
            env.server.apply(ConfigStreamItem::Snapshot {
                config: config.clone(),
            });
            cx.notify();
            env.shell.sequence
        })?;

        // Resuming after a reconnect replays only what was missed.
        let input = match after_sequence {
            Some(sequence) => json!({ "afterSequence": sequence, "requestCompletionMarker": true }),
            None => json!({ "requestCompletionMarker": true }),
        };
        // Lives as long as this connection's shell stream.
        let _config = cx.spawn({
            let this = this.clone();
            let rpc = rpc.clone();
            async move |cx: &mut AsyncApp| {
                let mut retry = RETRY_INITIAL;
                while let Err(error) = follow_config(this.clone(), rpc.clone(), cx).await {
                    tracing::warn!("config stream ended: {error:#}");
                    cx.background_executor().timer(retry).await;
                    retry = (retry * 2).min(RETRY_MAX);
                }
            }
        });
        let mut updates = rpc.subscribe::<_, ShellStreamItem>(shell::SUBSCRIBE, &input);
        let mut synchronized = false;
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
                    if matches!(item, ShellStreamItem::Synchronized) {
                        synchronized = true;
                    }
                    let snapshot = matches!(item, ShellStreamItem::Snapshot { .. });
                    let threads_changed = snapshot
                        || matches!(
                            item,
                            ShellStreamItem::ThreadUpdated { .. }
                                | ShellStreamItem::ThreadRemoved { .. }
                        );
                    if env.shell.apply(item) {
                        if threads_changed {
                            let notices = env
                                .notifications
                                .update(&env.shell, synchronized && !snapshot);
                            for notice in notices {
                                crate::notifications::present(notice, cx);
                            }
                        }
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

/// Subscribes before reading the settings so no update falls between them.
async fn follow_config(
    this: WeakEntity<LocalEnvironment>,
    rpc: RpcClient,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let mut items = rpc.subscribe::<_, ConfigStreamItem>(config::SUBSCRIBE, &json!({}));
    let settings = rpc
        .call::<_, Value>(config::GET_SETTINGS, &json!({}))
        .await?;
    let settings = serde_json::from_value(settings)?;
    this.update(cx, |env, cx| {
        env.server.settings = Some(settings);
        cx.notify();
    })?;
    while let Some(item) = items.next().await {
        let item = item?;
        this.update(cx, |env, cx| {
            if env.server.apply(item) {
                cx.notify();
            }
        })?;
    }
    Err(anyhow!("server config stream ended"))
}

const CLIENT_LABEL: &str = "T3 Code desktop";

async fn connect(
    http: reqwest::Client,
    http_base: Url,
    bootstrap_token: String,
) -> anyhow::Result<(
    RpcClient,
    t3_client::history::HistoryClient,
    t3_client::attachments::Client,
)> {
    let metadata = ClientMetadata {
        label: CLIENT_LABEL.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
    };
    let access =
        auth::exchange_bootstrap_credential(&http, &http_base, &bootstrap_token, &metadata).await?;
    let url = auth::websocket_url(&http, &http_base, &access.access_token, &metadata).await?;
    let attachments = t3_client::attachments::Client::new(http.clone(), http_base.clone());
    let history = t3_client::history::HistoryClient::new(http, http_base, access.access_token);
    Ok((RpcClient::connect(&url).await?, history, attachments))
}

fn set_state(
    this: &WeakEntity<LocalEnvironment>,
    cx: &mut AsyncApp,
    state: ConnectionState,
) -> anyhow::Result<()> {
    this.update(cx, |env, cx| {
        if !matches!(state, ConnectionState::Connected) {
            env.rpc = None;
            env.history = None;
        }
        env.state = state;
        cx.notify();
    })
}
