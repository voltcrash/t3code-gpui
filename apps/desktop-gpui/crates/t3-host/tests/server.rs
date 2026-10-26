//! Drives the real server from source, the way the desktop app will.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use t3_client::auth::{self, ClientMetadata};
use t3_client::config::{ConfigStreamItem, ServerState};
use t3_client::projects;
use t3_client::shell::{ShellState, ShellStreamItem};
use t3_client::thread::{ThreadState, ThreadStreamItem};
use t3_client::{RpcClient, RpcError};
use t3_host::{Backend, BackendLaunch};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn launches_authenticates_and_streams_from_the_real_server() {
    let repo = repo_root();
    let home =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("t3-home-{}", std::process::id()));
    let review_workspace = home.join("review-workspace");
    prepare_review_workspace(&review_workspace).await;
    let seeded =
        tokio::process::Command::new(std::env::var_os("T3_NODE").unwrap_or_else(|| "node".into()))
            .current_dir(repo.join("apps/server"))
            .args([
                "--input-type=module",
                "--eval",
                include_str!("history-fixture.mjs"),
            ])
            .arg(home.join("userdata/statev2.sqlite"))
            .arg(&review_workspace)
            .output()
            .await
            .unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let checkpoint_workspace = home.join("checkpoint-workspace");
    let seeded =
        tokio::process::Command::new(std::env::var_os("T3_NODE").unwrap_or_else(|| "node".into()))
            .current_dir(repo.join("apps/server"))
            .args([
                "--input-type=module",
                "--eval",
                include_str!("checkpoint-fixture.mjs"),
            ])
            .arg(home.join("userdata/statev2.sqlite"))
            .arg(&checkpoint_workspace)
            .output()
            .await
            .unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let mut backend = Backend::spawn(BackendLaunch {
        node: std::env::var_os("T3_NODE").map_or_else(|| "node".into(), PathBuf::from),
        entry: repo.join("apps/server/src/bin.ts"),
        cwd: repo.clone(),
        t3_home: home.clone(),
        port: None,
    })
    .await
    .unwrap();
    // First start runs every migration, which is slow on a cold CI runner.
    backend.wait_ready(Duration::from_secs(120)).await.unwrap();

    let http = reqwest::Client::new();
    let metadata = ClientMetadata {
        label: "T3 Code GPUI test".into(),
        app_version: "0.0.0".into(),
    };
    let token = auth::exchange_bootstrap_credential(
        &http,
        backend.http_base(),
        &backend.bootstrap_token(),
        &metadata,
    )
    .await
    .unwrap();
    let url = auth::websocket_url(&http, backend.http_base(), &token.access_token, &metadata)
        .await
        .unwrap();
    let client = RpcClient::connect(&url).await.unwrap();
    verifies_progressive_history(&client, &http, backend.http_base(), &token.access_token).await;
    verifies_review(&client, &review_workspace).await;
    verifies_checkpoint_review(&client, &checkpoint_workspace).await;

    let config: Value = client.call("server.getConfig", &json!({})).await.unwrap();
    assert!(
        config["environment"]["environmentId"].is_string(),
        "{config}"
    );

    let mut updates = client.subscribe::<_, ConfigStreamItem>("subscribeServerConfig", &json!({}));
    let first = updates.next().await.unwrap().unwrap();
    assert!(matches!(first, ConfigStreamItem::Snapshot { .. }));
    let mut server = ServerState::default();
    server.apply(first);

    let unknown = client
        .call::<_, Value>("no.such.method", &json!({}))
        .await
        .unwrap_err();
    assert!(matches!(unknown, RpcError::Defect(_)), "{unknown:?}");

    // The commands decode: the orchestrator, not the schema, rejects them.
    assert!(t3_client::commands::supported(&config));
    let missing = "thread-that-does-not-exist";
    for command in [
        t3_client::commands::stop_thread(missing),
        t3_client::commands::send_message(
            missing,
            "hello",
            Some(&json!({ "instanceId": "codex", "model": "gpt-6-astra" })),
        ),
        t3_client::commands::set_runtime_mode(missing, "approval-required"),
        t3_client::commands::respond_to_approval(missing, "request", "decline", Some("no")),
        t3_client::commands::respond_to_user_input(
            missing,
            "request",
            serde_json::Map::from_iter([("q".to_owned(), json!("answer"))]),
        ),
        t3_client::commands::dismiss_user_input(missing, "request"),
    ] {
        let rejected = t3_client::commands::dispatch(&client, &command)
            .await
            .unwrap_err();
        assert_eq!(
            rejected.failure_tag(),
            Some("OrchestrationV2DispatchCommandError"),
            "{rejected:?}"
        );
        assert!(rejected.detail().contains(missing), "{rejected:?}");
    }

    // New-thread defaults read the real settings and provider shapes.
    let settings: Value = client.call("server.getSettings", &json!({})).await.unwrap();
    serde_json::from_value::<t3_client::providers::Settings>(settings).unwrap();
    client
        .call::<_, Value>(
            "server.updateSettings",
            &json!({
                "patch": { "defaultRuntimeMode": "approval-required" },
            }),
        )
        .await
        .unwrap();
    while let Some(item) = updates.next().await {
        let item = item.unwrap();
        let settings_updated = matches!(item, ConfigStreamItem::SettingsUpdated { .. });
        server.apply(item);
        if settings_updated {
            break;
        }
    }
    assert_eq!(
        server
            .settings
            .as_ref()
            .unwrap()
            .default_runtime_mode
            .as_deref(),
        Some("approval-required")
    );
    drop(updates);

    // Change a real thread's mode and await its persisted event, without
    // starting a provider process or sending a prompt.
    let project: Value = client
        .call(
            "projects.mutate",
            &json!({
                "type": "project.create", "commandId": "create-mode-project",
                "projectId": "mode-project", "title": "Mode test",
                "workspaceRoot": home.join("project"), "createWorkspaceRootIfMissing": true,
            }),
        )
        .await
        .unwrap();
    let thread_id = "mode-test-thread";
    t3_client::commands::dispatch(
        &client,
        &json!({
            "type": "thread.create", "commandId": "create-mode-test",
            "threadId": thread_id, "projectId": project["id"],
            "createdBy": "user", "creationSource": "web", "title": "Mode test",
            "modelSelection": { "instanceId": "codex", "model": "gpt-6-astra" },
            "runtimeMode": "full-access", "interactionMode": "default",
            "branch": null, "worktreePath": null,
        }),
    )
    .await
    .unwrap();
    let mut thread_updates = client.subscribe::<_, ThreadStreamItem>(
        t3_client::thread::SUBSCRIBE,
        &json!({ "threadId": thread_id }),
    );
    let mut thread = ThreadState::default();
    thread.apply(thread_updates.next().await.unwrap().unwrap());
    assert_eq!(
        thread
            .projection
            .as_ref()
            .unwrap()
            .settings()
            .unwrap()
            .runtime_mode,
        "full-access"
    );
    t3_client::commands::dispatch(
        &client,
        &t3_client::commands::set_runtime_mode(thread_id, "approval-required"),
    )
    .await
    .unwrap();
    while let Some(item) = thread_updates.next().await {
        thread.apply(item.unwrap());
        if thread
            .projection
            .as_ref()
            .unwrap()
            .settings()
            .unwrap()
            .runtime_mode
            == "approval-required"
        {
            break;
        }
    }
    assert_eq!(
        thread
            .projection
            .as_ref()
            .unwrap()
            .settings()
            .unwrap()
            .runtime_mode,
        "approval-required"
    );
    drop(thread_updates);
    verifies_thread_lifecycle(&client, thread_id).await;
    assert_eq!(
        t3_client::providers::from_config(&config).len(),
        config["providers"].as_array().map_or(0, Vec::len),
        "every provider snapshot decodes"
    );
    let (_, launch) = t3_client::commands::launch_thread(&t3_client::commands::NewThread {
        project_id: "project-that-does-not-exist",
        model_selection: &json!({ "instanceId": "codex", "model": "gpt-6-astra" }),
        runtime_mode: "full-access",
        text: "hello",
    });
    let rejected = client
        .call::<_, Value>(t3_client::commands::LAUNCH_THREAD, &launch)
        .await
        .unwrap_err();
    assert!(matches!(rejected, RpcError::Failure(_)), "{rejected:?}");

    verifies_project_lifecycle(&client, &home).await;

    // The socket survives an interrupted stream and a failed call.
    let again: Value = client.call("server.getConfig", &json!({})).await.unwrap();
    assert_eq!(
        again["environment"]["environmentId"],
        config["environment"]["environmentId"]
    );

    backend.shutdown(Duration::from_secs(10)).await;
    assert!(!client.closed().await.is_empty());
    let _ = std::fs::remove_dir_all(home);
}

async fn verifies_checkpoint_review(client: &RpcClient, cwd: &std::path::Path) {
    use t3_client::checkpoint::{self, Diff};
    let mut stream = client.subscribe::<_, ThreadStreamItem>(
        t3_client::thread::SUBSCRIBE,
        &t3_client::thread::subscribe_input("history-fixture", None),
    );
    let mut thread = ThreadState::default();
    while let Some(item) = stream.next().await {
        thread.apply(item.unwrap());
        if thread.synchronized {
            break;
        }
    }
    let turns = checkpoint::turns(&thread.projection.as_ref().unwrap().checkpoints);
    assert_eq!(
        turns.iter().map(|turn| turn.count).collect::<Vec<_>>(),
        [2, 1]
    );
    let mut state = t3_client::review::State::new(true);
    // This RPC addresses the checkpoint's persisted environment-side scope, without a client cwd.
    let request = state.checkpoint_request("history-fixture", &turns[0]);
    let latest: Diff = client
        .call(checkpoint::TURN_DIFF, &request.input)
        .await
        .unwrap();
    assert!(latest.matches(&request.input));
    assert!(latest.diff.contains("first checkpoint"));
    assert!(latest.diff.contains("second checkpoint"));
    assert!(latest.diff.contains("rename from helper.rs"));
    assert!(latest.diff.contains("Binary files"));
    assert!(!latest.diff.contains("live uncaptured edit"));
    assert!(!latest.diff.contains("spacing.rs"));
    assert!(!latest.truncated());
    state.ignore_whitespace = false;
    let whitespace_request = state.checkpoint_request("history-fixture", &turns[0]);
    assert!(!state.accepts(&request));
    let whitespace: Diff = client
        .call(checkpoint::TURN_DIFF, &whitespace_request.input)
        .await
        .unwrap();
    assert!(whitespace.diff.contains("spacing.rs"));
    assert_ne!(whitespace.hash(), latest.hash());
    let first_request = state.checkpoint_request("history-fixture", &turns[1]);
    let first: Diff = client
        .call(checkpoint::TURN_DIFF, &first_request.input)
        .await
        .unwrap();
    assert!(first.diff.contains("before checkpoint"));
    assert!(first.diff.contains("first checkpoint"));
    assert!(!first.diff.contains("second checkpoint"));

    let refs: Value =
        serde_json::from_slice(&std::fs::read(cwd.join("refs.json")).unwrap()).unwrap();
    let latest_ref = refs["refs"][2].as_str().unwrap();
    fixture_git(cwd, &["update-ref", "-d", latest_ref]).await;
    let missing = client
        .call::<_, Diff>(checkpoint::TURN_DIFF, &request.input)
        .await
        .unwrap_err();
    assert_eq!(missing.failure_tag(), Some("OrchestrationGetTurnDiffError"));
    fixture_git(
        cwd,
        &[
            "update-ref",
            latest_ref,
            refs["commits"][2].as_str().unwrap(),
        ],
    )
    .await;
    let restored: Diff = client
        .call(checkpoint::TURN_DIFF, &request.input)
        .await
        .unwrap();
    assert_eq!(restored.diff, latest.diff);
    drop(stream);
}

async fn fixture_git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = tokio::process::Command::new("git")
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", cwd.join(".fixture-gitconfig"))
        .args([
            "-c",
            "user.name=T3 fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

async fn prepare_review_workspace(cwd: &std::path::Path) {
    std::fs::create_dir_all(cwd).unwrap();
    std::fs::write(cwd.join(".fixture-gitconfig"), "").unwrap();
    std::fs::write(cwd.join(".gitignore"), ".fixture-gitconfig\n").unwrap();
    std::fs::write(
        cwd.join("app.rs"),
        "fn main() {\n    println!(\"before\");\n}\n",
    )
    .unwrap();
    std::fs::write(cwd.join("spacing.rs"), "let value=1;\n").unwrap();
    std::fs::write(cwd.join("notes.md"), "Project notes\n").unwrap();
    std::fs::write(cwd.join("asset.bin"), [0, 1, 2, 3]).unwrap();
    fixture_git(cwd, &["init", "--initial-branch=gpui"]).await;
    fixture_git(cwd, &["add", "."]).await;
    fixture_git(cwd, &["commit", "-m", "Initial fixture"]).await;
    fixture_git(cwd, &["switch", "-c", "review-fixture"]).await;
    fixture_git(
        cwd,
        &[
            "config",
            "branch.review-fixture.t3-merge-base-ref",
            "refs/heads/gpui",
        ],
    )
    .await;
    std::fs::write(cwd.join("committed.md"), "A committed branch change.\n").unwrap();
    fixture_git(cwd, &["add", "committed.md"]).await;
    fixture_git(cwd, &["commit", "-m", "Branch change"]).await;
    std::fs::write(
        cwd.join("app.rs"),
        "fn main() {\n    println!(\"after\");\n}\n",
    )
    .unwrap();
    std::fs::write(cwd.join("spacing.rs"), "let value = 1;\n").unwrap();
    std::fs::write(cwd.join("asset.bin"), [0, 4, 5, 6]).unwrap();
    std::fs::write(cwd.join("new file.rs"), "pub fn added() {}\n").unwrap();
    std::fs::write(
        cwd.join("staged.md"),
        "Staged changes are also uncommitted.\n",
    )
    .unwrap();
    fixture_git(cwd, &["mv", "notes.md", "guide.md"]).await;
    fixture_git(cwd, &["add", "staged.md"]).await;
}

async fn verifies_review(client: &RpcClient, cwd: &std::path::Path) {
    use t3_client::review::{self, Preview, Scope};
    let index_before = fixture_git(cwd, &["diff", "--cached", "--binary"]).await;
    let mut state = review::State::new(true);
    state.set_workspace(Some(cwd.to_string_lossy().into_owned()));
    let request = state.preview_request().unwrap();
    let preview: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    assert!(state.apply(&request, preview));
    assert!(state.source().unwrap().diff.contains("committed.md"));
    state.select_scope(Scope::Uncommitted);
    let dirty = state.source().unwrap();
    assert!(!dirty.diff.contains("committed.md"));
    assert!(dirty.diff.contains("println!(\"after\")"));
    assert!(
        !dirty
            .files
            .as_ref()
            .unwrap()
            .iter()
            .any(|file| file.path == "spacing.rs")
    );
    let renamed = dirty
        .files
        .as_ref()
        .unwrap()
        .iter()
        .find(|file| file.path == "guide.md")
        .unwrap();
    assert_eq!(renamed.previous_path.as_deref(), Some("notes.md"));
    assert!(
        dirty
            .files
            .as_ref()
            .unwrap()
            .iter()
            .any(|file| file.path == "new file.rs")
    );
    assert!(dirty.diff.contains("Binary files"));
    assert!(dirty.diff.contains("staged.md"));
    state.select_file(Some("guide.md".into()));
    let file = state.file_request().unwrap();
    let renamed: Preview = client.call(review::PREVIEW, &file.input).await.unwrap();
    let patch = &renamed
        .sources
        .iter()
        .find(|source| source.kind == Scope::Uncommitted.kind())
        .unwrap()
        .diff;
    assert!(patch.contains("rename from notes.md"));
    assert!(!patch.contains("app.rs"));
    state.ignore_whitespace = false;
    let request = state.preview_request().unwrap();
    let whitespace: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    state.apply(&request, whitespace);
    assert!(
        state
            .source()
            .unwrap()
            .files
            .as_ref()
            .unwrap()
            .iter()
            .any(|file| file.path == "spacing.rs")
    );
    let large = format!("{}\nlast_line_sentinel\n", "large line\n".repeat(20000));
    std::fs::write(cwd.join("large.txt"), large).unwrap();
    let request = state.preview_request().unwrap();
    let overview: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    state.apply(&request, overview);
    assert!(state.source().unwrap().truncated);
    state.select_file(Some("large.txt".into()));
    let request = state.file_request().unwrap();
    let file: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    let source = file
        .sources
        .iter()
        .find(|source| source.kind == Scope::Uncommitted.kind())
        .unwrap();
    assert!(!source.truncated);
    assert!(source.diff.contains("last_line_sentinel"));
    std::fs::write(cwd.join("oversize.txt"), "oversize line\n".repeat(100000)).unwrap();
    let request = state.preview_request().unwrap();
    let overview: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    state.apply(&request, overview);
    state.select_file(Some("oversize.txt".into()));
    let request = state.file_request().unwrap();
    let file: Preview = client.call(review::PREVIEW, &request.input).await.unwrap();
    assert!(
        file.sources
            .iter()
            .find(|source| source.kind == Scope::Uncommitted.kind())
            .unwrap()
            .truncated
    );
    assert_eq!(
        fixture_git(cwd, &["diff", "--cached", "--binary"]).await,
        index_before,
        "review must not change the Git index"
    );
}

async fn verifies_progressive_history(
    client: &RpcClient,
    http: &reqwest::Client,
    base: &url::Url,
    access_token: &str,
) {
    let mut updates = client.subscribe::<_, ThreadStreamItem>(
        t3_client::thread::SUBSCRIBE,
        &t3_client::thread::subscribe_input("history-fixture", None),
    );
    let mut state = ThreadState::default();
    while !state.synchronized {
        state.apply(updates.next().await.unwrap().unwrap());
    }
    let initial = state.timeline().len();
    assert!(initial > 0 && initial < 70);
    assert!(state.partial);
    let sequence = state.sequence;
    let history =
        t3_client::history::HistoryClient::new(http.clone(), base.clone(), access_token.into());
    verifies_thread_find(client, &history, &state).await;
    let mut pages = 0;
    while let Some(request) = state.history_request() {
        let page = history
            .page("history-fixture", &request.cursor)
            .await
            .unwrap();
        assert_eq!(page.snapshot_sequence, sequence.unwrap());
        assert!(state.apply_history(&request, page));
        pages += 1;
        assert!(pages < 10, "history must make progress");
    }
    assert!(pages >= 2, "exercise multiple opaque cursors");
    assert_eq!(state.sequence, sequence, "history never skips live events");
    assert_eq!(state.timeline().len(), 70);
    assert_eq!(state.timeline().first().unwrap().id, "user_message-1");
    assert_eq!(state.timeline().last().unwrap().id, "assistant_message-35");
    assert!(!state.partial);
    assert!(matches!(
        history.page("history-fixture", "invalid cursor").await,
        Err(t3_client::history::HistoryError::Status(400))
    ));
    let unauthenticated =
        t3_client::history::HistoryClient::new(http.clone(), base.clone(), "invalid".into());
    assert!(matches!(
        unauthenticated
            .page("history-fixture", "invalid cursor")
            .await,
        Err(t3_client::history::HistoryError::Status(401))
    ));
}

async fn verifies_thread_find(
    client: &RpcClient,
    history: &t3_client::history::HistoryClient,
    recent: &ThreadState,
) {
    use t3_client::find::{self, SearchResult};
    let mut search = find::State::default();
    search.set_query("NEEDLE");
    let first = search.begin(0).unwrap();
    let result: SearchResult = client
        .call(find::SEARCH, &first.input("history-fixture"))
        .await
        .unwrap();
    assert_eq!(
        result.total_matches, 2,
        "search rendered text, excluding link destinations"
    );
    assert_eq!(result.snapshot_sequence, recent.sequence.unwrap());
    let selected = result.selected.as_ref().unwrap();
    assert_eq!(selected.entry_id, "assistant_message-1");
    assert_eq!(selected.occurrence, 0);
    assert!(
        !recent
            .timeline()
            .iter()
            .any(|entry| entry.id == selected.entry_id)
    );
    let cursor = recent.history_request().unwrap().cursor;
    let targeted = history
        .page_through("history-fixture", &cursor, Some(&selected.entry_id))
        .await
        .unwrap();
    assert!(
        targeted
            .items
            .iter()
            .any(|row| row.source_item_id == selected.entry_id)
    );
    search.apply(&first, result);
    let next = search.begin(1).unwrap();
    let second: SearchResult = client
        .call(find::SEARCH, &next.input("history-fixture"))
        .await
        .unwrap();
    assert_eq!(second.active_index, 1);
    assert_eq!(second.selected.as_ref().unwrap().occurrence, 1);
    search.apply(&next, second);
    let wrap = search.begin(1).unwrap();
    let wrapped: SearchResult = client
        .call(find::SEARCH, &wrap.input("history-fixture"))
        .await
        .unwrap();
    assert_eq!(wrapped.active_index, 0);
    search.apply(&wrap, wrapped);
    let previous = search.begin(-1).unwrap();
    let mut stream = client
        .subscribe::<_, SearchResult>(find::SEARCH_STREAM, &previous.input("history-fixture"));
    let mut complete = false;
    while let Some(result) = stream.next().await {
        let result = result.unwrap();
        if result.complete != Some(false) {
            assert_eq!(result.total_matches, 2);
            assert_eq!(result.active_index, 1);
            complete = true;
        }
    }
    assert!(complete);
    let no_matches: SearchResult = client
        .call(
            find::SEARCH,
            &json!({"threadId":"history-fixture","query":"absent query"}),
        )
        .await
        .unwrap();
    assert_eq!(no_matches.total_matches, 0);
    assert!(no_matches.selected.is_none());
}

async fn verifies_project_lifecycle(client: &RpcClient, home: &std::path::Path) {
    let config = projects::config(client).await.unwrap();
    assert!(!config.platform.is_empty());
    assert!(config.new_projects_root.is_some());
    let folder = home.join("folder-fixture");
    std::fs::create_dir_all(folder.join("child")).unwrap();
    std::fs::create_dir_all(folder.join(".hidden")).unwrap();
    std::fs::write(folder.join("file.txt"), "keep me").unwrap();
    let folders = projects::browse(
        client,
        &projects::directory(&folder.to_string_lossy()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        folders
            .entries
            .iter()
            .map(|folder| folder.name.as_str())
            .collect::<Vec<_>>(),
        [".hidden", "child"]
    );

    let mut updates =
        client.subscribe::<_, ShellStreamItem>(t3_client::shell::SUBSCRIBE, &json!({}));
    let mut shell = ShellState::default();
    shell.apply(updates.next().await.unwrap().unwrap());
    let registered = projects::add(client, &folder.to_string_lossy())
        .await
        .unwrap();
    assert_eq!(registered.title, "folder-fixture");
    while shell.project(&registered.id).is_none() {
        shell.apply(updates.next().await.unwrap().unwrap());
    }
    let duplicate = projects::add(client, &folder.to_string_lossy())
        .await
        .unwrap_err();
    assert!(matches!(duplicate, RpcError::Failure(_)), "{duplicate:?}");
    projects::remove(client, &registered.id).await.unwrap();
    while shell.project(&registered.id).is_some() {
        shell.apply(updates.next().await.unwrap().unwrap());
    }
    assert_eq!(
        std::fs::read_to_string(folder.join("file.txt")).unwrap(),
        "keep me"
    );

    let created = projects::create(client, "Native project").await.unwrap();
    while shell.project(&created.project_id).is_none() {
        shell.apply(updates.next().await.unwrap().unwrap());
    }
    assert_eq!(
        shell.project(&created.project_id).unwrap().title,
        "Native project"
    );
    let root = PathBuf::from(&created.workspace_root);
    assert!(root.join(".git").is_dir());
    assert!(root.join("README.md").is_file());
    projects::remove(client, &created.project_id).await.unwrap();
    while shell.project(&created.project_id).is_some() {
        shell.apply(updates.next().await.unwrap().unwrap());
    }
    assert!(root.join("README.md").is_file());
}

async fn verifies_thread_lifecycle(client: &RpcClient, id: &str) {
    use t3_client::thread_actions::Action;
    let mut updates =
        client.subscribe::<_, ShellStreamItem>(t3_client::shell::SUBSCRIBE, &json!({}));
    let mut archive_updates = client.subscribe::<_, t3_client::archives::StreamItem>(
        t3_client::archives::SUBSCRIBE,
        &json!({}),
    );
    let mut archive = t3_client::archives::State::default();
    archive.apply(archive_updates.next().await.unwrap().unwrap());
    let mut shell = ShellState::default();
    shell.apply(updates.next().await.unwrap().unwrap());
    for action in [
        Action::Rename("Renamed native thread".into()),
        Action::Pin,
        Action::Unpin,
        Action::Settle,
        Action::Unsettle,
        Action::Archive,
        Action::Rename("Renamed archived thread".into()),
        Action::Restore,
        Action::Archive,
        Action::Delete,
    ] {
        t3_client::commands::dispatch(client, &action.command(id))
            .await
            .unwrap();
        loop {
            tokio::select! {
                item = updates.next() => { shell.apply(item.unwrap().unwrap()); },
                item = archive_updates.next() => { archive.apply(item.unwrap().unwrap()); },
            }
            let active = shell.threads.get(id);
            let archived = archive.threads.get(id);
            let applied = match &action {
                Action::Rename(title) => active
                    .or(archived)
                    .is_some_and(|thread| thread.title == *title),
                Action::Pin => active.is_some_and(|thread| thread.pinned_at.is_some()),
                Action::Unpin => active.is_some_and(|thread| thread.pinned_at.is_none()),
                Action::Settle => active
                    .is_some_and(|thread| thread.settled_override.as_deref() == Some("settled")),
                Action::Unsettle => active
                    .is_some_and(|thread| thread.settled_override.as_deref() == Some("active")),
                Action::Archive => active.is_none() && archived.is_some(),
                Action::Restore => active.is_some() && archived.is_none(),
                Action::Delete => active.is_none() && archived.is_none(),
            };
            if applied {
                break;
            }
        }
    }
}
