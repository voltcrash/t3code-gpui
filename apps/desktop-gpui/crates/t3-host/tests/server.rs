//! Drives the real server from source, the way the desktop app will.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use t3_client::auth::{self, ClientMetadata};
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

    let config: Value = client.call("server.getConfig", &json!({})).await.unwrap();
    assert!(
        config["environment"]["environmentId"].is_string(),
        "{config}"
    );

    let mut updates = client.subscribe::<_, Value>("subscribeServerConfig", &json!({}));
    let first = updates.next().await.unwrap().unwrap();
    assert_eq!(first["type"], "snapshot");
    drop(updates);

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
        t3_client::commands::send_message(missing, "hello"),
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
