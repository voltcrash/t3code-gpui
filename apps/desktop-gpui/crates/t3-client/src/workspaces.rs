//! Draft workspace choices encode the existing launch strategy; Git stays on the server.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{RpcClient, RpcError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Strategy {
    Root {
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    Worktree {
        #[serde(rename = "baseRef")]
        base_ref: String,
        #[serde(rename = "startFromOrigin", default)]
        start_from_origin: bool,
    },
    ExistingWorktree {
        #[serde(rename = "worktreePath")]
        worktree_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
}

impl Default for Strategy {
    fn default() -> Self {
        Self::Root { branch: None }
    }
}

impl Strategy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Root { .. } => "Local",
            Self::Worktree { .. } => "New worktree",
            Self::ExistingWorktree { .. } => "Existing worktree",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub is_default: bool,
    #[serde(default)]
    pub is_remote: bool,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Branches {
    pub refs: Vec<Branch>,
    pub is_repo: bool,
    pub has_primary_remote: bool,
    pub next_cursor: Option<u64>,
}

pub async fn branches(
    rpc: &RpcClient,
    cwd: &str,
    query: &str,
    cursor: Option<u64>,
) -> Result<Branches, RpcError> {
    let mut input = json!({"cwd":cwd, "limit":100, "includeMatchingRemoteRefs":true, "refresh":cursor.is_none() && query.trim().is_empty()});
    if !query.trim().is_empty() {
        input["query"] = query.trim().into();
    }
    if let Some(cursor) = cursor {
        input["cursor"] = cursor.into();
    }
    rpc.call("vcs.listRefs", &input).await
}

#[derive(Debug)]
pub struct BranchSelection {
    pub strategy: Strategy,
    pub checkout_cwd: Option<String>,
}

pub fn select_branch(root: &str, current: &Strategy, branch: &Branch) -> BranchSelection {
    if matches!(current, Strategy::Worktree { .. }) {
        return BranchSelection {
            strategy: Strategy::Worktree {
                base_ref: branch.name.clone(),
                start_from_origin: matches!(
                    current,
                    Strategy::Worktree {
                        start_from_origin: true,
                        ..
                    }
                ),
            },
            checkout_cwd: None,
        };
    }
    let active = match current {
        Strategy::ExistingWorktree { worktree_path, .. } => worktree_path.as_str(),
        _ => root,
    };
    let destination = branch.worktree_path.as_deref().unwrap_or(active);
    let strategy = if crate::projects::same_path(root, destination) {
        Strategy::Root {
            branch: Some(branch.name.clone()),
        }
    } else {
        Strategy::ExistingWorktree {
            worktree_path: destination.into(),
            branch: Some(branch.name.clone()),
        }
    };
    BranchSelection {
        strategy,
        checkout_cwd: crate::projects::same_path(active, destination).then(|| active.into()),
    }
}

pub fn default_base(branches: &[Branch]) -> String {
    branches
        .iter()
        .find(|branch| branch.is_default)
        .or_else(|| branches.iter().find(|branch| branch.current))
        .map_or_else(|| "HEAD".into(), |branch| branch.name.clone())
}

pub fn project_file_mode(result: &Value) -> Option<bool> {
    if result["exists"] == false || result["truncated"] == true {
        return None;
    }
    let contents = result["contents"].as_str()?;
    let value = lenient_json(contents)?;
    match value["defaultThreadEnvMode"].as_str()? {
        "worktree" => Some(true),
        "local" => Some(false),
        _ => None,
    }
}

fn lenient_json(contents: &str) -> Option<Value> {
    let mut bytes = contents.as_bytes().to_vec();
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => index += 2,
            b'"' => {
                quoted = !quoted;
                index += 1;
            }
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    bytes[index] = b' ';
                    index += 1;
                }
            }
            b'/' if !quoted && bytes.get(index + 1) == Some(&b'*') => {
                bytes[index] = b' ';
                bytes[index + 1] = b' ';
                index += 2;
                while index + 1 < bytes.len() && &bytes[index..index + 2] != b"*/" {
                    bytes[index] = b' ';
                    index += 1;
                }
                if index + 1 >= bytes.len() {
                    return None;
                }
                bytes[index] = b' ';
                bytes[index + 1] = b' ';
                index += 2;
            }
            _ => index += 1,
        }
    }
    index = 0;
    quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => index += 2,
            b'"' => {
                quoted = !quoted;
                index += 1;
            }
            b',' if !quoted => {
                if bytes[index + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|byte| matches!(byte, b'}' | b']'))
                {
                    bytes[index] = b' ';
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    serde_json::from_slice(&bytes).ok()
}

pub fn default_worktree(
    settings: &crate::providers::Settings,
    project: &crate::shell::Project,
    from_file: Option<bool>,
    new_projects_root: Option<&str>,
    has_threads: bool,
) -> bool {
    let mode = |value: &str| match value {
        "local" => Some(false),
        "worktree" => Some(true),
        _ => None,
    };
    let stored = settings
        .project_settings_overrides
        .get(&project.id)
        .and_then(|value| value.get("defaultThreadEnvMode"));
    let project_override = match stored {
        Some(value) => value.as_str().and_then(mode),
        None if !settings.project_settings_folded => {
            project.default_thread_env_mode.as_deref().and_then(mode)
        }
        None => None,
    };
    if let Some(value) = project_override {
        return value;
    }
    let configured = settings.default_thread_env_mode.as_deref().and_then(mode);
    if configured.is_none() {
        return from_file.unwrap_or(false);
    }
    if !configured.unwrap_or(false) {
        return false;
    }
    if !has_threads
        && new_projects_root.is_some_and(|root| {
            crate::projects::parent(&project.workspace_root)
                .is_some_and(|parent| crate::projects::same_path(&parent, root))
        })
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_selection_distinguishes_root_existing_and_new_worktrees_without_normalizing_remote_paths()
     {
        let mut branch = Branch {
            name: "feature".into(),
            current: false,
            is_default: false,
            is_remote: false,
            worktree_path: Some("/worktrees/feature one".into()),
        };
        let existing = select_branch("/project", &Strategy::default(), &branch).strategy;
        assert_eq!(
            serde_json::to_value(&existing).unwrap(),
            json!({"type":"existing_worktree","worktreePath":"/worktrees/feature one","branch":"feature"})
        );
        assert_eq!(
            serde_json::to_value(
                select_branch(
                    "/project",
                    &Strategy::Worktree {
                        base_ref: "main".into(),
                        start_from_origin: false
                    },
                    &branch
                )
                .strategy
            )
            .unwrap(),
            json!({"type":"worktree","baseRef":"feature","startFromOrigin":false})
        );
        let active = Strategy::ExistingWorktree {
            worktree_path: "/worktrees/feature one".into(),
            branch: Some("feature".into()),
        };
        branch.worktree_path = None;
        let selection = select_branch("/project", &active, &branch);
        assert_eq!(
            selection.checkout_cwd.as_deref(),
            Some("/worktrees/feature one")
        );
        assert!(matches!(
            selection.strategy,
            Strategy::ExistingWorktree { .. }
        ));
        branch.worktree_path = Some("/project".into());
        let selection = select_branch("/project", &active, &branch);
        assert_eq!(selection.checkout_cwd, None);
        assert!(matches!(selection.strategy, Strategy::Root { .. }));
        branch.worktree_path = Some("C:\\Projects\\App\\".into());
        assert_eq!(
            select_branch("c:/projects/app", &Strategy::default(), &branch).strategy,
            Strategy::Root {
                branch: Some("feature".into())
            }
        );
        assert_eq!(
            serde_json::to_value(Strategy::default()).unwrap(),
            json!({"type":"root"})
        );
        for strategy in [
            Strategy::default(),
            existing,
            Strategy::Worktree {
                base_ref: "origin/main".into(),
                start_from_origin: true,
            },
        ] {
            let model = json!({"instanceId":"codex", "model":"gpt-6-astra"});
            let (_, input) = crate::commands::launch_thread(&crate::commands::NewThread {
                workspace: &strategy,
                project_id: "project",
                model_selection: &model,
                runtime_mode: "full-access",
                interaction_mode: "default",
                text: "hello",
                attachments: &[],
            });
            assert_eq!(
                input["workspaceStrategy"],
                serde_json::to_value(strategy).unwrap()
            );
        }
    }

    #[test]
    fn workspace_defaults_follow_project_environment_and_repository_inheritance() {
        let mut project: crate::shell::Project = serde_json::from_value(
            json!({"id":"project", "title":"Project", "workspaceRoot":"/projects/new"}),
        )
        .unwrap();
        let mut settings: crate::providers::Settings =
            serde_json::from_value(json!({"defaultThreadEnvMode":"worktree"})).unwrap();
        assert!(default_worktree(
            &settings,
            &project,
            Some(false),
            None,
            false
        ));
        assert!(!default_worktree(
            &settings,
            &project,
            None,
            Some("/projects"),
            false
        ));
        assert!(default_worktree(
            &settings,
            &project,
            None,
            Some("/projects"),
            true
        ));
        project.default_thread_env_mode = Some("local".into());
        assert!(!default_worktree(&settings, &project, None, None, true));
        settings
            .project_settings_overrides
            .insert("project".into(), json!({"defaultThreadEnvMode":null}));
        assert!(default_worktree(&settings, &project, None, None, true));
        settings
            .project_settings_overrides
            .insert("project".into(), json!({"defaultThreadEnvMode":"worktree"}));
        assert!(default_worktree(
            &settings,
            &project,
            None,
            Some("/projects"),
            false
        ));
        settings.project_settings_overrides.clear();
        settings.project_settings_folded = true;
        settings.default_thread_env_mode = None;
        assert!(default_worktree(
            &settings,
            &project,
            Some(true),
            Some("/projects"),
            false
        ));
        assert!(!default_worktree(&settings, &project, None, None, true));
    }

    #[test]
    fn repository_defaults_accept_comments_trailing_commas_and_quoted_comment_markers() {
        let file = json!({"contents": "{ // workspace\n\"url\":\"https://example.com/*value*/\", /* café */ \"defaultThreadEnvMode\":\"worktree\", }"});
        assert_eq!(project_file_mode(&file), Some(true));
        for file in [
            json!({"contents":"{\"defaultThreadEnvMode\":\"local\"}","truncated":true}),
            json!({"contents":"{\"defaultThreadEnvMode\":\"worktree\"}","exists":false}),
            json!({"contents":"{/* incomplete"}),
            json!({"contents":"{\"defaultThreadEnvMode\":\"future\"}"}),
        ] {
            assert_eq!(project_file_mode(&file), None);
        }
    }
}
