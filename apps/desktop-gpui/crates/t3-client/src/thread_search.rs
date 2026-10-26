//! Sidebar search follows Sidebar.logic.ts and the existing searchThreads RPC.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::shell::{PullRequest, ThreadShell};

pub const SEARCH: &str = "orchestration.searchThreads";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub thread_id: String,
    pub project_id: String,
    pub source: String,
    pub snippet: String,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchResult {
    pub matches: Vec<Match>,
}
#[derive(Debug, Clone)]
pub struct Request {
    pub query: String,
    generation: u64,
}
impl Request {
    pub fn input(&self) -> Value {
        json!({"query":self.query})
    }
}
#[derive(Debug, Default)]
pub struct State {
    pub query: String,
    pub matches: Vec<Match>,
    generation: u64,
}
impl State {
    pub fn set_query(&mut self, query: &str) -> bool {
        let query = query.trim();
        if query == self.query {
            return false;
        }
        self.query = query.into();
        self.invalidate();
        true
    }
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.matches.clear();
    }
    pub fn begin(&mut self) -> Option<Request> {
        if !(2..=200).contains(&self.query.encode_utf16().count()) {
            return None;
        }
        self.generation += 1;
        Some(Request {
            query: self.query.clone(),
            generation: self.generation,
        })
    }
    pub fn is_current(&self, request: &Request) -> bool {
        self.generation == request.generation && self.query == request.query
    }
    pub fn apply(&mut self, request: &Request, result: SearchResult) -> bool {
        if !self.is_current(request) {
            return false;
        }
        self.matches = result.matches;
        true
    }
    /// Preserve the supplied shelf order within each group, with title/PR
    /// matches before message-only matches, as searchSidebarThreads does.
    pub fn results<'a>(
        &'a self,
        threads: impl IntoIterator<Item = &'a ThreadShell>,
    ) -> Vec<(&'a ThreadShell, Option<&'a Match>)> {
        let query = self.query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let matches: HashMap<_, _> = self
            .matches
            .iter()
            .map(|found| (found.thread_id.as_str(), found))
            .collect();
        let mut title = Vec::new();
        let mut content = Vec::new();
        for thread in threads {
            if thread.archived_at.is_some() || thread.is_subagent() {
                continue;
            }
            let found = matches
                .get(thread.id.as_str())
                .copied()
                .filter(|found| found.project_id == thread.project_id);
            if title_match(thread, &query) {
                title.push((thread, found));
            } else if let Some(found) = found {
                content.push((thread, Some(found)));
            }
        }
        title.extend(content);
        title
    }
}

fn title_match(thread: &ThreadShell, query: &str) -> bool {
    let matches = |value: &str| value.to_lowercase().contains(query);
    let pr_matches = |pr: &PullRequest, include_title: bool| {
        matches(&format!("#{}", pr.number))
            || matches(&format!("{}#{}", pr.repository, pr.number))
            || matches(&pr.url)
            || (include_title
                && pr
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.title.as_deref())
                    .is_some_and(matches))
    };
    matches(&thread.title)
        || thread
            .branch_pull_request
            .as_ref()
            .is_some_and(|pr| pr_matches(pr, false))
        || if thread.pull_requests.is_empty() {
            thread
                .linked_pull_request
                .as_ref()
                .is_some_and(|pr| pr_matches(pr, false))
        } else {
            thread
                .pull_requests
                .iter()
                .filter(|pr| pr.source.as_deref() != Some("stack-dismissed"))
                .any(|pr| pr_matches(pr, true))
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn thread(id: &str, title: &str, extra: Value) -> ThreadShell {
        let mut value = json!({"id":id,"projectId":"project","title":title,"status":"idle","createdAt":"2026-10-26","updatedAt":"2026-10-26"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }
    fn found(id: &str) -> Match {
        Match {
            thread_id: id.into(),
            project_id: "project".into(),
            source: "assistant".into(),
            snippet: "Needle in conversation".into(),
        }
    }
    #[test]
    fn query_changes_clear_message_results_and_reject_stale_replies_and_reconnects() {
        let mut state = State::default();
        state.set_query("  needle  ");
        let old = state.begin().unwrap();
        assert_eq!(old.input(), json!({"query":"needle"}));
        state.set_query("new query");
        assert!(!state.apply(
            &old,
            SearchResult {
                matches: vec![found("old")]
            }
        ));
        let current = state.begin().unwrap();
        assert!(state.apply(
            &current,
            SearchResult {
                matches: vec![found("current")]
            }
        ));
        state.invalidate();
        assert!(state.matches.is_empty());
        assert!(!state.apply(
            &current,
            SearchResult {
                matches: vec![found("current")]
            }
        ));
        state.set_query("");
        assert!(
            state
                .results([&thread("a", "anything", json!({}))])
                .is_empty()
        );
    }
    #[test]
    fn message_search_uses_the_existing_utf16_query_limits() {
        let mut state = State::default();
        for (query, valid) in [
            ("a".to_owned(), false),
            ("ab".into(), true),
            ("😀".into(), true),
            ("é".repeat(200), true),
            ("😀".repeat(101), false),
        ] {
            state.set_query(&query);
            assert_eq!(state.begin().is_some(), valid);
        }
    }
    #[test]
    fn local_titles_and_prs_rank_before_message_matches_and_hidden_threads_are_excluded() {
        let threads = [
            thread("body", "Something else", json!({})),
            thread("title", "Needle in title", json!({})),
            thread(
                "pr",
                "Linked work",
                json!({"pullRequests":[{"number":42,"repository":"org/repo","url":"https://example.com/pull/42","snapshot":{"title":"Needle in PR"}}]}),
            ),
            thread(
                "hidden",
                "Needle",
                json!({"pullRequests":[],"archivedAt":"2026-10-26"}),
            ),
            thread(
                "agent",
                "Needle",
                json!({"lineage":{"relationshipToParent":"subagent"}}),
            ),
            thread(
                "wrong-project",
                "Unrelated",
                json!({"projectId":"elsewhere"}),
            ),
            thread(
                "dismissed",
                "Unrelated",
                json!({"pullRequests":[{"number":1,"repository":"org/repo","url":"needle","source":"stack-dismissed"}]}),
            ),
        ];
        let mut state = State::default();
        state.set_query("NEEDLE");
        let request = state.begin().unwrap();
        state.apply(
            &request,
            SearchResult {
                matches: vec![found("body"), found("wrong-project")],
            },
        );
        let result = state.results(&threads);
        assert_eq!(
            result
                .iter()
                .map(|(thread, _)| thread.id.as_str())
                .collect::<Vec<_>>(),
            ["title", "pr", "body"]
        );
        assert!(result[0].1.is_none());
        assert!(result[2].1.is_some());
        state.set_query("#42");
        assert_eq!(state.results(&threads)[0].0.id, "pr");
    }
}
