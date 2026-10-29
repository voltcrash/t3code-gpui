//! Archived threads have their own subscription and sequence, independent of
//! the active shell. Subscribe only when the archive is first opened.

use crate::shell::ThreadShell;
use serde::Deserialize;
use std::collections::HashMap;

pub const SUBSCRIBE: &str = "orchestration.subscribeArchivedShell";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum StreamItem {
    #[serde(rename = "snapshot")]
    Snapshot { snapshot: Snapshot },
    #[serde(rename = "thread.updated")]
    Updated {
        sequence: u64,
        thread: Box<ThreadShell>,
    },
    #[serde(rename = "thread.removed", rename_all = "camelCase")]
    Removed { sequence: u64, thread_id: String },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default)]
pub struct State {
    pub sequence: Option<u64>,
    pub threads: HashMap<String, ThreadShell>,
}

impl State {
    pub fn apply(&mut self, item: StreamItem) -> bool {
        let sequence = match item {
            StreamItem::Snapshot { snapshot } => {
                self.sequence = Some(snapshot.snapshot_sequence);
                self.threads = snapshot
                    .threads
                    .into_iter()
                    .map(|thread| (thread.id.clone(), thread))
                    .collect();
                return true;
            }
            StreamItem::Updated { sequence, .. } | StreamItem::Removed { sequence, .. } => sequence,
            StreamItem::Unknown => return false,
        };
        if self.sequence.is_some_and(|current| sequence <= current) {
            return false;
        }
        self.sequence = Some(sequence);
        match item {
            StreamItem::Updated { thread, .. } => {
                self.threads.insert(thread.id.clone(), *thread);
            }
            StreamItem::Removed { thread_id, .. } => {
                self.threads.remove(&thread_id);
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(value: serde_json::Value) -> StreamItem {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn archive_owns_its_sequence_and_replaces_reconnect_snapshots() {
        let thread = json!({"id":"archived-1","projectId":"project-1","title":"Archived","status":"idle","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","archivedAt":"2026-01-01T00:00:00Z"});
        let mut state = State::default();
        assert!(state.apply(item(
            json!({"kind":"snapshot","snapshot":{"snapshotSequence":10,"threads":[thread.clone()]}})
        )));
        assert!(!state.apply(item(
            json!({"kind":"thread.removed","sequence":9,"threadId":"archived-1"})
        )));
        assert!(state.threads.contains_key("archived-1"));
        assert!(state.apply(item(
            json!({"kind":"thread.removed","sequence":11,"threadId":"archived-1"})
        )));
        assert!(state.threads.is_empty());
        assert!(state.apply(item(
            json!({"kind":"thread.updated","sequence":12,"thread":thread})
        )));
        assert!(state.threads.contains_key("archived-1"));
        assert!(state.apply(item(
            json!({"kind":"snapshot","snapshot":{"snapshotSequence":20,"threads":[]}})
        )));
        assert!(state.threads.is_empty());
    }
}
