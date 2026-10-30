//! Archived threads load on demand and follow changes from the shared shell stream.

use crate::shell::{ShellStreamItem, ThreadLocation, ThreadShell};
use serde::Deserialize;
use std::collections::HashMap;

pub const GET_SNAPSHOT: &str = "orchestration.getArchivedShellSnapshot";

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
    pending: Option<Vec<StreamItem>>,
}

impl State {
    pub fn begin_snapshot(&mut self) {
        self.pending = Some(Vec::new());
    }

    pub fn cancel_snapshot(&mut self) {
        self.pending = None;
    }

    pub fn finish_snapshot(&mut self, snapshot: Snapshot) {
        // A shell event may arrive while the snapshot RPC is in flight.
        let pending = self.pending.take().unwrap_or_default();
        self.apply(StreamItem::Snapshot { snapshot });
        for item in pending {
            self.apply(item);
        }
    }

    pub fn apply_shell(&mut self, item: &ShellStreamItem) -> bool {
        let item = match item {
            ShellStreamItem::ThreadUpdated {
                sequence,
                location,
                thread,
            } => {
                if *location == ThreadLocation::Archive {
                    StreamItem::Updated {
                        sequence: *sequence,
                        thread: thread.clone(),
                    }
                } else {
                    StreamItem::Removed {
                        sequence: *sequence,
                        thread_id: thread.id.clone(),
                    }
                }
            }
            ShellStreamItem::ThreadRemoved {
                sequence,
                location: ThreadLocation::Archive,
                thread_id,
            } => StreamItem::Removed {
                sequence: *sequence,
                thread_id: thread_id.clone(),
            },
            _ => return false,
        };
        if let Some(pending) = &mut self.pending {
            pending.push(item.clone());
        }
        if self.sequence.is_none() {
            return false;
        }
        self.apply(item)
    }

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
    fn archive_replaces_snapshots_and_replays_newer_shell_updates() {
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
        state.begin_snapshot();
        let update = serde_json::from_value(json!({"kind":"thread.updated","sequence":22,"location":"archive","thread":thread.clone()})).unwrap();
        assert!(state.apply_shell(&update));
        state.finish_snapshot(Snapshot {
            snapshot_sequence: 21,
            threads: vec![],
        });
        assert!(state.threads.contains_key("archived-1"));
        state.begin_snapshot();
        let restored = serde_json::from_value(
            json!({"kind":"thread.updated","sequence":23,"location":"active","thread":thread.clone()}),
        )
        .unwrap();
        assert!(state.apply_shell(&restored));
        state.finish_snapshot(Snapshot {
            snapshot_sequence: 22,
            threads: vec![serde_json::from_value(thread).unwrap()],
        });
        assert!(state.threads.is_empty());
    }
}
