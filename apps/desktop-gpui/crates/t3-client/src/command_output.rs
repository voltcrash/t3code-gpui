//! Lazy command detail reads use the source item, including inherited history.
use crate::thread::{ProjectedTurnItem, TurnItem};
use serde::{Deserialize, Serialize};

pub const GET: &str = "orchestration.getTurnItem";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub thread_id: String,
    pub item_id: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub request: Request,
    pub omitted: bool,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
}

pub fn source(row: &ProjectedTurnItem) -> Option<Source> {
    let item = &row.item;
    if item.kind != "command_execution" {
        return None;
    }
    Some(Source {
        request: Request {
            thread_id: row.source_thread_id.clone(),
            item_id: row.source_item_id.clone(),
            // Match turnItemDetailRevision: one read while running, then another
            // when it finishes, rather than a request for every streamed update.
            revision: if matches!(
                item.status.as_str(),
                "idle" | "pending" | "running" | "waiting"
            ) {
                "live".into()
            } else {
                item.updated_at.clone()
            },
        },
        omitted: item.fields.get("outputOmitted") == Some(&serde_json::Value::Bool(true)),
        output: output(item),
        exit_code: item
            .fields
            .get("exitCode")
            .and_then(serde_json::Value::as_i64),
    })
}

#[derive(Deserialize)]
pub struct Response {
    pub item: Option<TurnItem>,
}

impl Request {
    pub fn accepts(&self, item: &TurnItem) -> bool {
        item.thread_id == self.thread_id
            && item.id == self.item_id
            && item.kind == "command_execution"
            && (self.revision == "live" || item.updated_at == self.revision)
    }
}

pub fn output(item: &TurnItem) -> Option<String> {
    let text = item.str("output")?.to_owned();
    if text.trim().is_empty() {
        return None;
    }
    if text.trim_start().starts_with("{\"stdout\"")
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        && let (Some(stdout), Some(stderr), Some(_)) = (
            value["stdout"].as_str(),
            value["stderr"].as_str(),
            value["interrupted"].as_bool(),
        )
    {
        let text = [stdout, stderr]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return (!text.is_empty()).then_some(text);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn row() -> ProjectedTurnItem {
        serde_json::from_value(json!({"position":0,"visibility":"inherited","sourceThreadId":"parent","sourceItemId":"original","item":{"id":"visible","threadId":"child","type":"command_execution","ordinal":1,"status":"running","updatedAt":"first","input":"pwd","outputOmitted":true}})).unwrap()
    }
    #[test]
    fn inherited_reads_use_source_identity_and_one_revision_until_completion() {
        let mut row = row();
        let first = source(&row).unwrap();
        assert_eq!(
            serde_json::to_value(&first.request).unwrap(),
            json!({"threadId":"parent","itemId":"original","revision":"live"})
        );
        row.item.updated_at = "second".into();
        assert_eq!(source(&row).unwrap(), first);
        row.item.status = "completed".into();
        assert_eq!(source(&row).unwrap().request.revision, "second");
    }
    #[test]
    fn detail_results_must_match_identity_kind_and_finished_revision() {
        let mut row = row();
        row.item.status = "completed".into();
        let request = source(&row).unwrap().request;
        let mut item = row.item;
        item.id = "original".into();
        item.thread_id = "parent".into();
        assert!(request.accepts(&item));
        item.updated_at = "newer".into();
        assert!(!request.accepts(&item));
        item.updated_at = "first".into();
        item.thread_id = "other".into();
        assert!(!request.accepts(&item));
        item.thread_id = "parent".into();
        item.kind = "dynamic_tool".into();
        assert!(!request.accepts(&item));
    }
    #[test]
    fn outputs_preserve_raw_text_and_unpack_only_the_known_command_envelope() {
        let mut item = row().item;
        for (raw, expected) in [
            ("café\n\u{1b}[31merror", "café\n\u{1b}[31merror"),
            (
                r#"{"stdout":"out\n","stderr":"err","interrupted":false}"#,
                "out\n\nerr",
            ),
            (r#"{"stdout":"other JSON"}"#, r#"{"stdout":"other JSON"}"#),
        ] {
            item.fields.insert("output".into(), raw.into());
            assert_eq!(output(&item).as_deref(), Some(expected));
        }
        item.fields.insert("output".into(), " \n".into());
        assert_eq!(output(&item), None);
    }
}
