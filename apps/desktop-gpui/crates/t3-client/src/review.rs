//! Reads the existing review protocol. Git operations stay on the environment.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::shell::{ShellState, ThreadShell};

pub const PREVIEW: &str = "review.getDiffPreview";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Branch,
    Uncommitted,
}

impl Scope {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Branch => "branch-range",
            Self::Uncommitted => "working-tree",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Branch => "Changes",
            Self::Uncommitted => "Uncommitted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub kind: String,
    pub title: String,
    pub base_ref: Option<String>,
    pub diff: String,
    pub diff_hash: String,
    pub truncated: bool,
    pub files: Option<Vec<FileStat>>,
}

#[derive(Debug, Deserialize)]
pub struct Preview {
    pub sources: Vec<Source>,
}

pub fn workspace<'a>(shell: &'a ShellState, thread: &'a ThreadShell) -> Option<&'a str> {
    thread
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .or_else(|| {
            shell
                .project(&thread.project_id)
                .map(|project| project.workspace_root.as_str())
        })
}

#[derive(Clone, Debug)]
pub struct Request {
    generation: u64,
    pub input: Value,
}

#[derive(Debug, Default)]
pub struct State {
    pub cwd: Option<String>,
    pub scope: Scope,
    pub ignore_whitespace: bool,
    pub preview: Option<Preview>,
    pub selected_file: Option<String>,
    generation: u64,
}

impl State {
    pub fn new(ignore_whitespace: bool) -> Self {
        Self {
            ignore_whitespace,
            ..Default::default()
        }
    }
    pub fn set_workspace(&mut self, cwd: Option<String>) -> bool {
        if self.cwd == cwd {
            return false;
        }
        self.cwd = cwd;
        self.preview = None;
        self.selected_file = None;
        self.generation += 1;
        true
    }

    pub fn source(&self) -> Option<&Source> {
        self.preview
            .as_ref()?
            .sources
            .iter()
            .find(|source| source.kind == self.scope.kind())
    }

    pub fn select_scope(&mut self, scope: Scope) -> bool {
        if self.scope == scope {
            return false;
        }
        self.scope = scope;
        self.selected_file = None;
        self.generation += 1;
        true
    }

    pub fn select_file(&mut self, path: Option<String>) {
        self.selected_file = path;
        self.generation += 1;
    }

    pub fn preview_request(&mut self) -> Option<Request> {
        let cwd = self.cwd.as_ref()?;
        self.generation += 1;
        Some(Request {
            generation: self.generation,
            input: json!({ "cwd": cwd, "ignoreWhitespace": self.ignore_whitespace }),
        })
    }

    pub fn file_request(&mut self) -> Option<Request> {
        let source = self.source()?;
        let file = source
            .files
            .as_ref()?
            .iter()
            .find(|file| Some(&file.path) == self.selected_file.as_ref())?;
        let input = json!({
            "cwd": self.cwd.as_ref()?, "baseRef": source.base_ref,
            "ignoreWhitespace": self.ignore_whitespace,
            "file": { "path": file.path, "previousPath": file.previous_path, "sourceKind": source.kind },
        });
        // Optional baseRef is absent rather than null on the wire.
        let mut input = input;
        if source.base_ref.is_none() {
            input.as_object_mut()?.remove("baseRef");
        }
        self.generation += 1;
        Some(Request {
            generation: self.generation,
            input,
        })
    }

    pub fn accepts(&self, request: &Request) -> bool {
        self.generation == request.generation
    }

    pub fn apply(&mut self, request: &Request, preview: Preview) -> bool {
        if !self.accepts(request) {
            return false;
        }
        self.preview = Some(preview);
        let source = self.source();
        if self.selected_file.as_ref().is_some_and(|selected| {
            source
                .and_then(|source| source.files.as_ref())
                .is_none_or(|files| !files.iter().any(|file| &file.path == selected))
        }) {
            self.selected_file = None;
        }
        if self.selected_file.is_none() && self.source().is_some_and(|source| source.truncated) {
            self.selected_file = self
                .source()
                .and_then(|source| source.files.as_ref()?.first())
                .map(|file| file.path.clone());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(truncated: bool) -> Preview {
        serde_json::from_value(json!({ "sources": [{
            "kind":"branch-range", "title":"Changes", "baseRef":"gpui", "headRef":"feature",
            "diff":"patch", "diffHash":"hash", "truncated":truncated,
            "files":[{"path":"renamed file.rs", "previousPath":"old file.rs", "additions":2, "deletions":1}]
        }] })).unwrap()
    }

    #[test]
    fn changing_workspace_or_scope_rejects_in_flight_results() {
        let mut state = State::default();
        state.set_workspace(Some("remote/worktree".into()));
        let old = state.preview_request().unwrap();
        state.set_workspace(Some("other/worktree".into()));
        assert!(!state.apply(&old, preview(false)));
        let other = state.preview_request().unwrap();
        state.select_scope(Scope::Uncommitted);
        assert!(!state.apply(&other, preview(false)));
        assert!(state.preview.is_none());
    }

    #[test]
    fn truncated_overviews_choose_one_file_and_preserve_rename_metadata() {
        let mut state = State {
            ignore_whitespace: true,
            ..Default::default()
        };
        state.set_workspace(Some("D:\\remote worktree".into()));
        let request = state.preview_request().unwrap();
        assert!(state.apply(&request, preview(true)));
        assert_eq!(state.selected_file.as_deref(), Some("renamed file.rs"));
        let file = state.file_request().unwrap();
        assert_eq!(file.input["cwd"], "D:\\remote worktree");
        assert_eq!(file.input["baseRef"], "gpui");
        assert_eq!(file.input["ignoreWhitespace"], true);
        assert_eq!(
            file.input["file"],
            json!({"path":"renamed file.rs","previousPath":"old file.rs","sourceKind":"branch-range"})
        );
        state.select_file(None);
        assert!(!state.accepts(&file));
        assert!(state.file_request().is_none());
    }

    #[test]
    fn refresh_drops_removed_selection_and_ignores_unknown_sources() {
        let mut state = State::default();
        state.set_workspace(Some("/repo".into()));
        state.select_file(Some("removed".into()));
        let request = state.preview_request().unwrap();
        assert!(state.apply(&request, preview(false)));
        assert!(state.selected_file.is_none());
        state.select_scope(Scope::Uncommitted);
        assert!(state.source().is_none());
        let request = state.preview_request().unwrap();
        let unknown = serde_json::from_value(json!({"sources":[{"kind":"future","title":"Future","baseRef":null,"diff":"","diffHash":"h","truncated":false}]})).unwrap();
        state.apply(&request, unknown);
        assert!(state.source().is_none());
    }

    #[test]
    fn review_uses_the_environment_worktree_without_resolving_paths_locally() {
        let mut shell = ShellState::default();
        shell.projects.push(crate::shell::Project {
            id: "project".into(),
            title: "Remote project".into(),
            workspace_root: "D:\\project".into(),
            default_model_selection: None,
        });
        let mut thread: ThreadShell = serde_json::from_value(json!({
            "id":"thread", "projectId":"project", "title":"Worktree", "status":"idle",
            "createdAt":"2026-10-26", "updatedAt":"2026-10-26",
            "worktreePath":"D:\\worktrees\\feature"
        }))
        .unwrap();
        assert_eq!(workspace(&shell, &thread), Some("D:\\worktrees\\feature"));
        thread.worktree_path = Some(String::new());
        assert_eq!(workspace(&shell, &thread), Some("D:\\project"));
        shell.projects.clear();
        assert_eq!(workspace(&shell, &thread), None);
    }
}
