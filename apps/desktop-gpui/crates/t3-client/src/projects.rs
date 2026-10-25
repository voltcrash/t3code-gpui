//! Project commands and folder navigation from the existing client runtime.
//! Paths belong to the destination environment, never the UI machine.

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::shell::Project;
use crate::{RpcClient, RpcError};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub name: String,
    pub full_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folders {
    pub parent_path: String,
    pub entries: Vec<Folder>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    pub project_id: String,
    pub workspace_root: String,
    pub commit_error: Option<String>,
}

pub struct Config {
    pub initial_path: String,
    pub platform: String,
    pub new_projects_root: Option<String>,
    pub scratch_workspace_root: Option<String>,
}

pub async fn config(rpc: &RpcClient) -> Result<Config, RpcError> {
    let config: Value = rpc.call("server.getConfig", &json!({})).await?;
    let base = config["settings"]["addProjectBaseDirectory"]
        .as_str()
        .unwrap_or_default()
        .trim();
    Ok(Config {
        initial_path: if base.is_empty() {
            "~/".into()
        } else {
            directory(base)
        },
        platform: config["environment"]["platform"]["os"]
            .as_str()
            .unwrap_or_default()
            .into(),
        new_projects_root: config["newProjectsRoot"].as_str().map(str::to_owned),
        scratch_workspace_root: config["scratchWorkspaceRoot"].as_str().map(str::to_owned),
    })
}

pub async fn browse(rpc: &RpcClient, path: &str, cwd: Option<&str>) -> Result<Folders, RpcError> {
    let mut input = json!({ "partialPath": path });
    if let Some(cwd) = cwd {
        input["cwd"] = cwd.into();
    }
    rpc.call("filesystem.browse", &input).await
}

pub async fn add(rpc: &RpcClient, path: &str) -> Result<Project, RpcError> {
    rpc.call("projects.mutate", &json!({
        "type": "project.create", "commandId": Uuid::new_v4().to_string(),
        "projectId": Uuid::new_v4().to_string(), "title": title(path),
        "workspaceRoot": path, "createWorkspaceRootIfMissing": true, "defaultModelSelection": null,
    })).await
}

pub async fn create(rpc: &RpcClient, name: &str) -> Result<NewProject, RpcError> {
    rpc.call("projects.createNew", &json!({ "name": name.trim() }))
        .await
}

pub async fn remove(rpc: &RpcClient, project_id: &str) -> Result<Project, RpcError> {
    rpc.call(
        "projects.mutate",
        &json!({
            "type": "project.delete", "commandId": Uuid::new_v4().to_string(),
            "projectId": project_id, "force": true,
        }),
    )
    .await
}

fn windows(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || (bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes.len() == 2 || matches!(bytes[2], b'/' | b'\\')))
}

fn separator(path: &str) -> char {
    if windows(path) || (!path.starts_with('/') && path.contains('\\')) {
        '\\'
    } else {
        '/'
    }
}

fn root(path: &str) -> Option<(String, Vec<&str>)> {
    if path.starts_with("\\\\") {
        let mut parts = path.split(['/', '\\']).filter(|part| !part.is_empty());
        let host = parts.next()?;
        let share = parts.next()?;
        Some((format!("\\\\{host}\\{share}\\"), parts.collect()))
    } else if windows(path) {
        Some((
            format!("{}\\", &path[..2]),
            path[2..]
                .split(['/', '\\'])
                .filter(|part| !part.is_empty())
                .collect(),
        ))
    } else if path.starts_with('/') {
        Some((
            "/".into(),
            path.split('/').filter(|part| !part.is_empty()).collect(),
        ))
    } else {
        None
    }
}

/// `normalizeProjectPathForDispatch`: preserve roots and Unix backslashes.
pub fn normalize(path: &str) -> String {
    let path = path.trim();
    if path == "/" || path == "\\" || (path.len() == 3 && windows(path)) {
        return path.into();
    }
    let trimmed = if path.starts_with('/') {
        path.trim_end_matches('/')
    } else {
        path.trim_end_matches(['/', '\\'])
    };
    if trimmed.is_empty() {
        path.into()
    } else if trimmed.len() == 2 && windows(trimmed) && path.len() > 2 {
        format!("{trimmed}\\")
    } else {
        trimmed.into()
    }
}

pub fn same_path(left: &str, right: &str) -> bool {
    let key = |path: &str| {
        let normalized = normalize(path);
        if windows(&normalized) {
            normalized.replace('/', "\\").to_lowercase()
        } else {
            normalized
        }
    };
    !left.trim().is_empty() && key(left) == key(right)
}

pub fn directory(path: &str) -> String {
    let path = path.trim();
    if path.ends_with(separator(path)) || (windows(path) && path.ends_with('/')) {
        path.into()
    } else {
        format!("{path}{}", separator(path))
    }
}

pub fn parent(path: &str) -> Option<String> {
    let normalized = normalize(path);
    if let Some((root, mut parts)) = root(&normalized) {
        parts.pop()?;
        let separator = separator(&root).to_string();
        Some(if parts.is_empty() {
            root
        } else {
            format!("{root}{}{separator}", parts.join(&separator))
        })
    } else {
        let (head, _) = normalized.rsplit_once(separator(&normalized))?;
        Some(directory(head))
    }
}

pub fn title(path: &str) -> String {
    let normalized = normalize(path);
    if let Some((_, parts)) = root(&normalized) {
        parts.last().copied().unwrap_or(&normalized).into()
    } else {
        normalized
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(&normalized)
            .into()
    }
}

/// `resolveAddProjectPath`, including relative paths anchored to the open
/// project. This cannot use Rust's host-specific `Path` parser.
pub fn resolve(path: &str, cwd: Option<&str>, platform: &str) -> Result<String, &'static str> {
    let path = path.trim();
    if path.is_empty() {
        return Err("Enter a project path.");
    }
    if windows(path) && !platform.to_ascii_lowercase().starts_with("win") {
        return Err("Windows-style paths are only supported on Windows environments.");
    }
    let relative = path == "."
        || path == ".."
        || ["./", "../", ".\\", "..\\"]
            .iter()
            .any(|prefix| path.starts_with(prefix));
    if !relative {
        return Ok(normalize(path));
    }
    let cwd = cwd.ok_or("Relative paths require an active project in this environment.")?;
    let Some((base, mut parts)) = root(cwd) else {
        return Ok(normalize(path));
    };
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let separator = separator(&base).to_string();
    Ok(normalize(&format!("{base}{}", parts.join(&separator))))
}

/// The parent is fetched once; typing within it filters the cached children.
/// This mirrors `getFilesystemBrowsePath` and `filterFilesystemBrowseEntries`.
pub fn browse_query(path: &str) -> (String, String) {
    let path = path.trim();
    let backslash = separator(path) == '\\';
    let index = path.rfind(|ch| ch == '/' || (backslash && ch == '\\'));
    match index {
        Some(index) => (path[..index + 1].into(), path[index + 1..].into()),
        None => (path.into(), String::new()),
    }
}

pub fn is_browse_path(path: &str) -> bool {
    let path = path.trim();
    ["/", "~/", "./", "../", ".\\", "..\\"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || windows(path)
}

pub fn filter_folders<'a>(folders: &'a [Folder], query: &str) -> Vec<&'a Folder> {
    let lower = query.to_lowercase();
    folders
        .iter()
        .filter(|folder| {
            folder.name.to_lowercase().starts_with(&lower)
                && (!folder.name.starts_with('.') || query.starts_with('.'))
        })
        .collect()
}

pub fn removal_message(project: &Project, threads: usize) -> String {
    let subject = if threads == 0 {
        format!("Remove project \"{}\"?", project.title)
    } else {
        format!(
            "Remove project \"{}\" and delete its {threads} thread{}?",
            project.title,
            if threads == 1 { "" } else { "s" }
        )
    };
    format!(
        "{subject}\nPath: {}\nThis permanently clears conversation history, including archived threads. The files on disk are kept. This action cannot be undone.",
        project.workspace_root
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_identity_and_titles_follow_the_destination_path_syntax() {
        assert!(same_path(
            " C:/Users/Dev/Project/// ",
            "c:\\users\\dev\\project"
        ));
        assert!(!same_path("/Work/Project", "/work/project"));
        assert!(!same_path("", ""));
        assert_eq!(title("/work/a\\b///"), "a\\b");
        assert_eq!(title("C:\\Users\\Dev\\Project\\"), "Project");
        assert_eq!(title("/"), "/");
        assert_eq!(normalize("C:"), "C:");
        assert!(same_path("C:\\", "c:/"));
    }

    #[test]
    fn relative_projects_resolve_on_the_environment_without_crossing_its_root() {
        assert_eq!(
            resolve("../other/", Some("/work/current"), "linux"),
            Ok("/work/other".into())
        );
        assert_eq!(
            resolve("../../../../other", Some("C:\\work\\current"), "win32"),
            Ok("C:\\other".into())
        );
        assert_eq!(
            resolve("../other", Some("\\\\server\\share\\current"), "win32"),
            Ok("\\\\server\\share\\other".into())
        );
        assert!(resolve("./other", None, "linux").is_err());
        assert!(resolve("C:\\work", None, "linux").is_err());
        assert_eq!(
            resolve(" ~/project/ ", None, "darwin"),
            Ok("~/project".into())
        );
    }

    #[test]
    fn browsing_preserves_windows_roots_and_does_not_treat_unix_backslashes_as_separators() {
        assert!(!is_browse_path("folder-name"));
        assert!(is_browse_path("~/folder"));
        assert_eq!(directory("C:"), "C:\\");
        assert_eq!(parent("C:\\work\\project\\"), Some("C:\\work\\".into()));
        assert_eq!(
            parent("\\\\server\\share\\project"),
            Some("\\\\server\\share\\".into())
        );
        assert_eq!(parent("C:\\"), None);
        assert_eq!(parent("/"), None);
        assert_eq!(browse_query("/work/a\\b"), ("/work/".into(), "a\\b".into()));
        assert_eq!(
            browse_query("C:\\work\\pro"),
            ("C:\\work\\".into(), "pro".into())
        );
    }

    #[test]
    fn folder_filtering_hides_dot_folders_until_their_prefix_is_typed() {
        let folders = [
            Folder {
                name: "Projects".into(),
                full_path: "/Projects".into(),
            },
            Folder {
                name: ".config".into(),
                full_path: "/.config".into(),
            },
        ];
        assert_eq!(filter_folders(&folders, "pro").len(), 1);
        assert_eq!(filter_folders(&folders, "").len(), 1);
        assert_eq!(filter_folders(&folders, ".c").len(), 1);
    }
}
