//! Where the server comes from. Development runs it from this checkout's
//! sources; packaged builds will bundle a Node runtime and the built server.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use t3_host::BackendLaunch;

pub fn development() -> anyhow::Result<BackendLaunch> {
    let repo = match std::env::var_os("T3_REPO_ROOT") {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.."),
    };
    let repo = repo
        .canonicalize()
        .with_context(|| format!("the repository root {} does not exist", repo.display()))?;
    let entry = repo.join("apps/server/src/bin.ts");
    if !entry.exists() {
        bail!(
            "{} is missing; is T3_REPO_ROOT a T3 Code checkout?",
            entry.display()
        );
    }
    Ok(BackendLaunch {
        node: std::env::var_os("T3_NODE").map_or_else(|| "node".into(), PathBuf::from),
        entry,
        cwd: repo.clone(),
        // Like `vp run dev`, a checkout keeps its own state so development
        // never touches the real ~/.t3.
        t3_home: std::env::var_os("T3_DESKTOP_HOME")
            .map_or_else(|| repo.join(".t3"), PathBuf::from),
        port: None,
    })
}
