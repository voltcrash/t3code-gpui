//! Source checkouts use their own state; installed apps use bundled Node and server files.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use t3_host::BackendLaunch;

#[derive(Default)]
struct Overrides {
    repo: Option<PathBuf>,
    home: Option<PathBuf>,
    node: Option<PathBuf>,
    ambient_home: Option<PathBuf>,
}

pub fn resolve() -> anyhow::Result<BackendLaunch> {
    let cwd = std::env::current_dir()?;
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    let path = |name| std::env::var_os(name).map(PathBuf::from);
    let overrides = Overrides {
        repo: path("T3_REPO_ROOT").map(absolute),
        home: path("T3_DESKTOP_HOME").map(absolute),
        ambient_home: path("T3CODE_HOME").map(absolute),
        node: path("T3_NODE").map(|node| {
            if node.components().count() > 1 {
                absolute(node)
            } else {
                node
            }
        }),
    };
    let user_home = path(if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(absolute);
    resolve_at(
        &std::env::current_exe()?,
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.."),
        user_home.as_deref(),
        overrides,
        cfg!(debug_assertions),
    )
}

fn resources_for(executable: &Path) -> anyhow::Result<PathBuf> {
    let directory = executable
        .parent()
        .context("desktop executable has no parent directory")?;
    if cfg!(target_os = "macos") {
        Ok(directory
            .parent()
            .context("desktop bundle has no Contents directory")?
            .join("Resources"))
    } else {
        Ok(directory.join("resources"))
    }
}

fn resolve_at(
    executable: &Path,
    source_root: &Path,
    user_home: Option<&Path>,
    overrides: Overrides,
    allow_development: bool,
) -> anyhow::Result<BackendLaunch> {
    let resources = resources_for(executable)?;
    if overrides.repo.is_some() || (allow_development && !resources.exists()) {
        let source = overrides.repo.as_deref().unwrap_or(source_root);
        let repo = source.canonicalize().with_context(|| {
            format!(
                "the development repository root {} does not exist",
                source.display()
            )
        })?;
        let entry = repo.join("apps/server/src/bin.ts");
        if !entry.is_file() {
            bail!(
                "{} is missing; is T3_REPO_ROOT a T3 Code checkout?",
                entry.display()
            );
        }
        return Ok(BackendLaunch {
            node: overrides.node.unwrap_or_else(|| "node".into()),
            entry,
            cwd: repo.clone(),
            // An ambient T3CODE_HOME must never put a development build on installed state.
            t3_home: overrides.home.unwrap_or_else(|| repo.join(".t3")),
            port: None,
        });
    }
    let entry = resources.join("server/apps/server/dist/bin.mjs");
    if !entry.is_file() {
        bail!(
            "the bundled T3 Code server is missing at {}",
            entry.display()
        );
    }
    let node = if let Some(node) = overrides.node {
        node
    } else {
        let node = resources.join(if cfg!(target_os = "windows") {
            "node/node.exe"
        } else {
            "node/bin/node"
        });
        if !node.is_file() {
            bail!("the bundled Node runtime is missing at {}", node.display());
        }
        node
    };
    let user_home = user_home
        .context("the user home directory is unavailable; cannot start the packaged server")?;
    Ok(BackendLaunch {
        node,
        entry,
        // Installed resource directories may be read-only; Electron also starts in the user's home.
        cwd: user_home.to_path_buf(),
        t3_home: overrides
            .home
            .or(overrides.ambient_home)
            .unwrap_or_else(|| user_home.join(".t3")),
        port: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture").unwrap();
    }

    fn executable(root: &Path) -> PathBuf {
        root.join(if cfg!(target_os = "macos") {
            "T3 Code.app/Contents/MacOS/t3-desktop"
        } else {
            "T3 Code/t3-desktop"
        })
    }

    fn bundle(executable: &Path) -> (PathBuf, PathBuf) {
        let resources = resources_for(executable).unwrap();
        let entry = resources.join("server/apps/server/dist/bin.mjs");
        let node = resources.join(if cfg!(target_os = "windows") {
            "node/node.exe"
        } else {
            "node/bin/node"
        });
        put(&entry);
        put(&node);
        (entry, node)
    }

    #[test]
    fn installed_resources_win_even_when_the_compiled_source_checkout_still_exists() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("checkout");
        put(&repo.join("apps/server/src/bin.ts"));
        let repo = repo.canonicalize().unwrap();
        let exe = executable(root.path());
        let (entry, node) = bundle(&exe);
        let launch =
            resolve_at(&exe, &repo, Some(root.path()), Overrides::default(), true).unwrap();
        assert_eq!(launch.entry, entry);
        assert_eq!(launch.node, node);
        assert_eq!(launch.cwd, root.path());
        assert_eq!(launch.t3_home, root.path().join(".t3"));
    }

    #[test]
    fn development_ignores_ambient_installed_state_and_explicit_repo_wins_over_a_bundle() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("checkout");
        put(&repo.join("apps/server/src/bin.ts"));
        let repo = repo.canonicalize().unwrap();
        let exe = executable(root.path());
        let launch = resolve_at(
            &exe,
            &repo,
            None,
            Overrides {
                ambient_home: Some(root.path().join("installed-home")),
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(launch.t3_home, repo.join(".t3"));
        bundle(&exe);
        let launch = resolve_at(
            &exe,
            root.path(),
            None,
            Overrides {
                repo: Some(repo.clone()),
                home: Some(root.path().join("sandbox")),
                node: Some("custom-node".into()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        assert_eq!(launch.entry, repo.join("apps/server/src/bin.ts"));
        assert_eq!(launch.node, PathBuf::from("custom-node"));
        assert_eq!(launch.t3_home, root.path().join("sandbox"));
    }

    #[test]
    fn release_and_partial_bundles_fail_instead_of_silently_using_source_or_path_node() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("checkout");
        put(&repo.join("apps/server/src/bin.ts"));
        let repo = repo.canonicalize().unwrap();
        let exe = executable(root.path());
        assert!(
            resolve_at(&exe, &repo, Some(root.path()), Overrides::default(), false)
                .unwrap_err()
                .to_string()
                .contains("bundled T3 Code server")
        );
        let (entry, node) = bundle(&exe);
        std::fs::remove_file(node).unwrap();
        assert!(
            resolve_at(&exe, &repo, Some(root.path()), Overrides::default(), true)
                .unwrap_err()
                .to_string()
                .contains("bundled Node runtime")
        );
        std::fs::remove_file(entry).unwrap();
        assert!(
            resolve_at(&exe, &repo, Some(root.path()), Overrides::default(), true)
                .unwrap_err()
                .to_string()
                .contains("bundled T3 Code server")
        );
    }

    #[test]
    fn packaged_overrides_keep_data_out_of_resources_and_allow_an_explicit_node() {
        let root = tempfile::tempdir().unwrap();
        let exe = executable(root.path());
        bundle(&exe);
        let home = root.path().join("sandbox");
        let ambient = root.path().join("ambient-home");
        let launch = resolve_at(
            &exe,
            root.path(),
            Some(root.path()),
            Overrides {
                home: Some(home.clone()),
                node: Some("custom-node".into()),
                ambient_home: Some(ambient.clone()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        assert_eq!(launch.t3_home, home);
        assert_eq!(launch.cwd, root.path());
        assert_eq!(launch.node, PathBuf::from("custom-node"));
        let launch = resolve_at(
            &exe,
            root.path(),
            Some(root.path()),
            Overrides {
                ambient_home: Some(ambient.clone()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        assert_eq!(launch.t3_home, ambient);
    }
}
