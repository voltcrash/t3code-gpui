//! Editor discovery and launch use the destination environment's existing API.

use serde_json::{Value, json};

use crate::shell::{Project, ThreadShell};
use crate::{RpcClient, RpcError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Editor {
    pub id: &'static str,
    pub label: &'static str,
}

// Order and labels mirror EDITORS in packages/contracts/src/editor.ts.
const EDITORS: &[(&str, &str)] = &[
    ("cursor", "Cursor"),
    ("trae", "Trae"),
    ("kiro", "Kiro"),
    ("vscode", "VS Code"),
    ("vscode-insiders", "VS Code Insiders"),
    ("vscodium", "VSCodium"),
    ("zed", "Zed"),
    ("antigravity", "Antigravity"),
    ("idea", "IntelliJ IDEA"),
    ("aqua", "Aqua"),
    ("clion", "CLion"),
    ("datagrip", "DataGrip"),
    ("dataspell", "DataSpell"),
    ("goland", "GoLand"),
    ("phpstorm", "PhpStorm"),
    ("pycharm", "PyCharm"),
    ("rider", "Rider"),
    ("rubymine", "RubyMine"),
    ("rustrover", "RustRover"),
    ("webstorm", "WebStorm"),
    ("file-manager", "File Manager"),
];

#[derive(Debug, Default)]
pub struct Catalog {
    pub available: Vec<Editor>,
}

impl Catalog {
    pub fn from_config(config: &Value) -> Self {
        let available = config["availableEditors"].as_array();
        let platform = config["environment"]["platform"]["os"].as_str();
        Self {
            available: EDITORS
                .iter()
                .filter(|(id, _)| available.is_some_and(|list| list.iter().any(|v| v == id)))
                .map(|&(id, label)| Editor {
                    id,
                    label: match (id, platform) {
                        ("file-manager", Some("darwin")) => "Finder",
                        ("file-manager", Some("win32")) => "File Explorer",
                        ("file-manager", _) => "Files",
                        _ => label,
                    },
                })
                .collect(),
        }
    }

    pub fn preferred(&self, saved: Option<&str>) -> Option<Editor> {
        self.available
            .iter()
            .find(|editor| Some(editor.id) == saved)
            .or_else(|| self.available.first())
            .copied()
    }
}

pub fn cwd<'a>(project: &'a Project, thread: Option<&'a ThreadShell>) -> Option<&'a str> {
    if thread.is_some_and(|thread| thread.project_id != project.id) {
        return None;
    }
    thread
        .and_then(|thread| thread.worktree_path.as_deref())
        .or(Some(project.workspace_root.as_str()))
        .filter(|path| !path.trim().is_empty())
}

pub async fn open(rpc: &RpcClient, cwd: &str, editor: Editor) -> Result<(), RpcError> {
    rpc.call(
        "shell.openInEditor",
        &json!({"cwd":cwd, "editor":editor.id}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_keeps_contract_order_and_falls_back_when_the_saved_editor_is_unavailable() {
        let catalog = Catalog::from_config(&json!({"availableEditors":[
            "future-editor", "file-manager", "zed", "vscode", "vscode", null
        ]}));
        assert_eq!(
            catalog.available.iter().map(|e| e.id).collect::<Vec<_>>(),
            ["vscode", "zed", "file-manager"]
        );
        for saved in [None, Some("cursor"), Some("future-editor")] {
            assert_eq!(catalog.preferred(saved).unwrap().id, "vscode");
        }
        assert_eq!(catalog.preferred(Some("zed")).unwrap().id, "zed");
        assert!(
            Catalog::from_config(&json!({}))
                .preferred(Some("zed"))
                .is_none()
        );
        for (platform, label) in [
            ("darwin", "Finder"),
            ("win32", "File Explorer"),
            ("linux", "Files"),
        ] {
            let catalog = Catalog::from_config(&json!({
                "availableEditors":["file-manager"], "environment":{"platform":{"os":platform}}
            }));
            assert_eq!(catalog.preferred(None).unwrap().label, label);
        }
    }

    #[test]
    fn the_destination_path_uses_the_thread_worktree_and_keeps_environment_paths_opaque() {
        let project: Project = serde_json::from_value(json!({
            "id":"project", "title":"Project", "workspaceRoot":"C:\\Projects\\My app"
        }))
        .unwrap();
        let mut thread: ThreadShell = serde_json::from_value(json!({
            "id":"thread", "projectId":"project", "title":"Thread", "status":"idle",
            "createdAt":"2026-10-30", "updatedAt":"2026-10-30",
            "worktreePath":"/remote/worktrees/my app"
        }))
        .unwrap();
        assert_eq!(cwd(&project, None), Some("C:\\Projects\\My app"));
        assert_eq!(
            cwd(&project, Some(&thread)),
            Some("/remote/worktrees/my app")
        );
        thread.worktree_path = None;
        assert_eq!(cwd(&project, Some(&thread)), Some("C:\\Projects\\My app"));
        thread.project_id = "another-project".into();
        assert_eq!(cwd(&project, Some(&thread)), None);
    }
}
