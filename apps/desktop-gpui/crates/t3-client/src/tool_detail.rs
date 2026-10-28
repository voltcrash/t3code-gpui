//! Lazy tool detail reads use the source item, including inherited history.
use crate::thread::{ProjectedTurnItem, TurnItem};
use serde::{Deserialize, Serialize};

pub const GET: &str = "orchestration.getTurnItem";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Dynamic,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Dynamic => "tool",
        }
    }
    fn matches(self, item: &TurnItem) -> bool {
        matches!(
            (self, item.kind.as_str()),
            (Self::Command, "command_execution") | (Self::Dynamic, "dynamic_tool")
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(skip)]
    pub kind: Kind,
    pub thread_id: String,
    pub item_id: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub request: Request,
    pub omitted: bool,
    pub input: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
}

pub fn source(row: &ProjectedTurnItem) -> Option<Source> {
    let item = &row.item;
    let kind = match item.kind.as_str() {
        "command_execution" => Kind::Command,
        "dynamic_tool" => Kind::Dynamic,
        _ => return None,
    };
    Some(Source {
        request: Request {
            kind,
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
        omitted: item.fields.get("outputOmitted") == Some(&serde_json::Value::Bool(true))
            || (kind == Kind::Dynamic && item.fields.get("input").is_some_and(summarized)),
        input: input(item),
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
            && self.kind.matches(item)
            && (self.revision == "live" || item.updated_at == self.revision)
    }
}

pub fn output(item: &TurnItem) -> Option<String> {
    if item.kind == "dynamic_tool" {
        return item.fields.get("output").and_then(tool_value);
    }
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

fn summarized(value: &serde_json::Value) -> bool {
    value["truncated"] == true && value["summary"].is_string()
}

pub fn input(item: &TurnItem) -> Option<String> {
    if item.kind != "dynamic_tool" {
        return None;
    }
    let value = item.fields.get("input")?;
    if summarized(value) {
        return None;
    }
    if value.is_string() {
        tool_value(value)
    } else {
        json_value(value)
    }
}

fn json_value(value: &serde_json::Value) -> Option<String> {
    if value.is_null()
        || value.as_object().is_some_and(|v| v.is_empty())
        || value.as_array().is_some_and(|v| v.is_empty())
    {
        return None;
    }
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_owned());
    }
    serde_json::to_string_pretty(value).ok()
}

fn tool_value(value: &serde_json::Value) -> Option<String> {
    let text = text_blocks(value, 0).or_else(|| json_value(value))?;
    if text.trim().is_empty() {
        return None;
    }
    // Match prettyJsonText without changing ordinary tool text or shell quoting.
    if matches!(text.trim_start().chars().next(), Some('{' | '['))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
    {
        return serde_json::to_string_pretty(&json).ok();
    }
    Some(text)
}

fn text_blocks(value: &serde_json::Value, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(blocks) = value.as_array() {
        return blocks
            .iter()
            .map(|block| text_blocks(block, depth + 1))
            .collect::<Option<Vec<_>>>()
            .map(|parts| {
                parts
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
    }
    let object = value.as_object()?;
    match value["type"].as_str() {
        Some("text") => return value["text"].as_str().map(str::to_owned),
        Some("image") => return Some("[image]".into()),
        Some("resource_link") => return value["uri"].as_str().map(str::to_owned),
        Some("resource") => {
            return value["resource"]["text"]
                .as_str()
                .or(value["resource"]["uri"].as_str())
                .map(str::to_owned);
        }
        _ => {}
    }
    let keys: Vec<_> = object
        .keys()
        .filter(|key| !matches!(key.as_str(), "isError" | "is_error"))
        .collect();
    if keys.len() == 1 && object.contains_key("content") {
        return text_blocks(&value["content"], depth + 1);
    }
    if keys.len() == 2 && object.contains_key("content") && object.contains_key("structuredContent")
    {
        return text_blocks(&value["content"], depth + 1).filter(|text| !text.trim().is_empty());
    }
    None
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
    #[test]
    fn summarized_arguments_trigger_detail_reads_and_preserve_explicit_values() {
        let mut row = row();
        row.item.kind = "dynamic_tool".into();
        row.item.fields.remove("outputOmitted");
        row.item.fields.insert(
            "input".into(),
            json!({"truncated":true,"summary":"large arguments"}),
        );
        let source = source(&row).unwrap();
        assert!(source.omitted);
        assert_eq!(source.request.kind, Kind::Dynamic);
        assert_eq!(source.input, None);
        row.item.fields.insert(
            "input".into(),
            json!({"empty":"","clear":null,"enabled":false,"count":0}),
        );
        let text = input(&row.item).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            row.item.fields["input"]
        );
        let mut physical = row.item.clone();
        physical.thread_id = "parent".into();
        physical.id = "original".into();
        assert!(source.request.accepts(&physical));
    }
    #[test]
    fn tool_content_blocks_extract_text_and_resources_without_rendering_markup() {
        let mut item = row().item;
        item.kind = "dynamic_tool".into();
        item.fields.insert("output".into(), json!({"content":[{"type":"text","text":"<b>café</b>"},{"type":"resource_link","uri":"file:///example"},{"type":"image","mimeType":"image/png","data":"placeholder"}],"isError":false}));
        assert_eq!(
            output(&item).as_deref(),
            Some("<b>café</b>\nfile:///example\n[image]")
        );
        item.fields.insert("output".into(), json!({"content":[]}));
        assert_eq!(output(&item), None);
    }
    #[test]
    fn structured_results_and_unrecognized_blocks_keep_their_data() {
        let mut item = row().item;
        item.kind = "dynamic_tool".into();
        for value in [
            json!({"result":{"count":0,"ok":false}}),
            json!({"content":[],"structuredContent":{"count":4}}),
            json!([{"type":"unknown","data":"retained"}]),
        ] {
            item.fields.insert("output".into(), value.clone());
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&output(&item).unwrap()).unwrap(),
                value
            );
        }
        item.fields.insert("output".into(), json!("{\"ok\":true}"));
        assert_eq!(output(&item).unwrap(), "{\n  \"ok\": true\n}");
    }
}
