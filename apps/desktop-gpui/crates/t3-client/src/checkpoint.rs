//! Turn review reads the server's checkpoint ordinals; it never compares local files.

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const TURN_DIFF: &str = "orchestration.getTurnDiff";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub run_id: Option<String>,
    pub app_run_ordinal: Option<u64>,
    pub status: String,
    pub captured_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub checkpoint_id: String,
    pub run_id: String,
    pub count: u64,
    pub status: String,
    pub captured_at: String,
}

/// Matches `deriveThreadCheckpointSummaries` and the diff panel's ordering.
/// Missing/stale checkpoints stay selectable so the server can explain their availability.
pub fn turns(checkpoints: &[Checkpoint]) -> Vec<Turn> {
    let mut turns: Vec<_> = checkpoints
        .iter()
        .filter_map(|checkpoint| {
            Some(Turn {
                checkpoint_id: checkpoint.id.clone(),
                run_id: checkpoint.run_id.clone()?,
                count: checkpoint.app_run_ordinal.filter(|count| *count > 0)?,
                status: checkpoint.status.clone(),
                captured_at: checkpoint.captured_at.clone(),
            })
        })
        .collect();
    turns.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| right.captured_at.cmp(&left.captured_at))
    });
    turns
}

pub fn input(thread_id: &str, turn: &Turn, ignore_whitespace: bool) -> Value {
    json!({
        "threadId": thread_id,
        "fromTurnCount": turn.count.saturating_sub(1),
        "toTurnCount": turn.count,
        "ignoreWhitespace": ignore_whitespace,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    pub thread_id: String,
    pub from_turn_count: u64,
    pub to_turn_count: u64,
    pub diff: String,
}

impl Diff {
    pub fn matches(&self, input: &Value) -> bool {
        input["threadId"].as_str() == Some(&self.thread_id)
            && input["fromTurnCount"].as_u64() == Some(self.from_turn_count)
            && input["toTurnCount"].as_u64() == Some(self.to_turn_count)
    }

    pub fn truncated(&self) -> bool {
        self.diff.ends_with("\n\n[truncated]")
    }

    pub fn hash(&self) -> String {
        hex::encode(Sha256::digest(self.diff.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_turns_exclude_baselines_and_sort_by_app_ordinal() {
        let checkpoints = serde_json::from_value::<Vec<Checkpoint>>(json!([
            {"id":"baseline", "runId":null, "appRunOrdinal":null, "status":"ready", "capturedAt":"2026-10-26"},
            {"id":"one", "runId":"run1", "appRunOrdinal":1, "status":"ready", "capturedAt":"2026-10-26"},
            {"id":"two", "runId":"run2", "appRunOrdinal":3, "status":"missing", "capturedAt":"2026-10-25"},
            {"id":"tool", "runId":"run2", "appRunOrdinal":null, "status":"ready", "capturedAt":"2026-10-26"}
        ])).unwrap();
        let turns = turns(&checkpoints);
        assert_eq!(
            turns.iter().map(|turn| turn.count).collect::<Vec<_>>(),
            [3, 1]
        );
        assert_eq!(turns[0].status, "missing");
        assert_eq!(
            input("thread", &turns[0], false),
            json!({
                "threadId":"thread", "fromTurnCount":2, "toTurnCount":3, "ignoreWhitespace":false
            })
        );
        assert_eq!(input("thread", &turns[1], true)["fromTurnCount"], 0);
    }

    #[test]
    fn diff_results_must_match_the_requested_thread_and_checkpoint_range() {
        let mut diff = Diff {
            thread_id: "thread".into(),
            from_turn_count: 2,
            to_turn_count: 3,
            diff: "patch".into(),
        };
        let input = json!({"threadId":"thread", "fromTurnCount":2, "toTurnCount":3});
        assert!(diff.matches(&input));
        diff.thread_id = "other".into();
        assert!(!diff.matches(&input));
        diff.thread_id = "thread".into();
        diff.to_turn_count = 4;
        assert!(!diff.matches(&input));
        diff.diff = "diff --git a/file b/file\n+literal [truncated]\n".into();
        assert!(!diff.truncated());
        diff.diff.push_str("\n\n[truncated]");
        assert!(diff.truncated());
    }
}
