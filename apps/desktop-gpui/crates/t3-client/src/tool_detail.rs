//! Lazy tool detail reads use the source item, including inherited history.
use crate::thread::{ProjectedTurnItem, TurnItem};
use serde::{Deserialize, Serialize};

pub const GET: &str = "orchestration.getTurnItem";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Dynamic,
    FileSearch,
    WebSearch,
    FileChange,
}
impl Kind {
    pub fn copy_label(self) -> &'static str {
        match self {
            Self::Command => "Copy command output",
            Self::Dynamic => "Copy tool output",
            Self::FileSearch | Self::WebSearch => "Copy search results",
            Self::FileChange => "Copy file changes",
        }
    }
    pub fn heading(self) -> &'static str {
        match self {
            Self::FileSearch | Self::WebSearch => "Results",
            Self::FileChange => "Changes",
            _ => "Output",
        }
    }
    pub fn is_search(self) -> bool {
        matches!(self, Self::FileSearch | Self::WebSearch)
    }
    fn matches(self, item: &TurnItem) -> bool {
        matches!(
            (self, item.kind.as_str()),
            (Self::Command, "command_execution")
                | (Self::Dynamic, "dynamic_tool")
                | (Self::FileSearch, "file_search")
                | (Self::WebSearch, "web_search")
                | (Self::FileChange, "file_change")
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
    pub images: Vec<crate::tool_images::Image>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
}

pub fn source(row: &ProjectedTurnItem) -> Option<Source> {
    let item = &row.item;
    let kind = match item.kind.as_str() {
        "command_execution" => Kind::Command,
        "dynamic_tool" => Kind::Dynamic,
        "file_search" => Kind::FileSearch,
        "web_search" => Kind::WebSearch,
        "file_change" => Kind::FileChange,
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
        images: crate::tool_images::images(item, &row.source_thread_id, &row.source_item_id),
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
    if item.kind == "file_change" {
        return file_changes(item);
    }
    if matches!(item.kind.as_str(), "file_search" | "web_search") {
        return search_results(item);
    }
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

fn file_changes(item: &TurnItem) -> Option<String> {
    let files: Vec<String> = item
        .fields
        .get("changes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| {
            let path = change["path"].as_str()?;
            let operation = change["operation"].as_str()?;
            Some(if let Some(old) = change["oldPath"].as_str() {
                format!("{operation} {old} → {path}")
            } else {
                format!("{operation} {path}")
            })
        })
        .collect();
    let files = if files.is_empty() {
        item.str("fileName")?.to_owned()
    } else {
        files.join("\n")
    };
    let mut blocks = vec![files];
    let counts: Vec<String> = [("additions", "+"), ("deletions", "-")]
        .into_iter()
        .filter_map(|(field, sign)| {
            item.fields
                .get(field)?
                .as_u64()
                .map(|count| format!("{sign}{count}"))
        })
        .collect();
    if !counts.is_empty() {
        blocks.push(format!("Lines: {}", counts.join(" ")));
    }
    if item.status == "failed"
        && let Some(patch) = item.str("diffStr").filter(|patch| !patch.trim().is_empty())
    {
        blocks.push(format!("Attempted patch\n{patch}"));
    }
    Some(blocks.join("\n\n"))
}

fn search_results(item: &TurnItem) -> Option<String> {
    let results = item.fields.get("results")?.as_array()?;
    let blocks: Vec<String> = results
        .iter()
        .filter_map(|result| {
            if item.kind == "file_search" {
                let name = result["fileName"].as_str()?;
                let location = result["line"]
                    .as_u64()
                    .map_or_else(|| name.to_owned(), |line| format!("{name}:{line}"));
                let preview = result["preview"]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty());
                Some(preview.map_or_else(|| location.clone(), |text| format!("{location}\n{text}")))
            } else {
                let title = result["title"]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty());
                let url = result["url"]
                    .as_str()
                    .filter(|text| !text.trim().is_empty());
                let snippet = result["snippet"]
                    .as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty());
                let text = [title, url, snippet]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(text)
            }
        })
        .collect();
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
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
    let images = crate::tool_images::image_blocks(value);
    let text = text_blocks(value, 0, &images).or_else(|| json_value(value))?;
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

fn text_blocks(
    value: &serde_json::Value,
    depth: usize,
    images: &[&serde_json::Value],
) -> Option<String> {
    if depth > 4 {
        return None;
    }
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(blocks) = value.as_array() {
        return blocks
            .iter()
            .map(|block| text_blocks(block, depth + 1, images))
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
        Some("image") => {
            return Some(
                if images.iter().any(|image| std::ptr::eq(*image, value)) {
                    ""
                } else {
                    "[image]"
                }
                .into(),
            );
        }
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
        return text_blocks(&value["content"], depth + 1, images);
    }
    if keys.len() == 2 && object.contains_key("content") && object.contains_key("structuredContent")
    {
        return text_blocks(&value["content"], depth + 1, images)
            .filter(|text| !text.trim().is_empty());
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
            Some("<b>café</b>\nfile:///example")
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
    #[test]
    fn file_search_results_keep_paths_locations_and_multiline_previews() {
        let mut row = row();
        row.item.kind = "file_search".into();
        row.item.fields.remove("outputOmitted");
        row.item.fields.insert("results".into(), json!([{"fileName":"src/café.rs","line":12,"column":4,"preview":"  first\n  second  "},{"fileName":"other.rs"}]));
        let source = source(&row).unwrap();
        assert_eq!(source.request.kind, Kind::FileSearch);
        assert!(!source.omitted);
        assert_eq!(
            source.output.as_deref(),
            Some("src/café.rs:12\nfirst\n  second\n\nother.rs")
        );
        row.item.fields.insert("results".into(), json!([]));
        assert_eq!(output(&row.item), None);
        row.item.fields.remove("results");
        assert_eq!(output(&row.item), None);
    }
    #[test]
    fn web_search_results_keep_titles_urls_and_snippets_without_markup_interpretation() {
        let mut row = row();
        row.item.kind = "web_search".into();
        row.item.fields.remove("outputOmitted");
        row.item.fields.insert("results".into(), json!([{"title":"<b>Reference</b>","url":"https://example.com/a?q=café","snippet":" Summary. "},{"url":"https://example.com/b"},{}]));
        let source = source(&row).unwrap();
        assert!(!source.omitted);
        assert!(source.request.kind.is_search());
        assert_eq!(
            source.output.as_deref(),
            Some(
                "<b>Reference</b>\nhttps://example.com/a?q=café\nSummary.\n\nhttps://example.com/b"
            )
        );
        row.item.fields.insert("results".into(), json!([]));
        assert_eq!(output(&row.item), None);
    }
    #[test]
    fn file_changes_keep_operations_rename_paths_and_only_supplied_counts() {
        let mut row = row();
        row.item.kind = "file_change".into();
        row.item.fields.remove("outputOmitted");
        row.item
            .fields
            .insert("fileName".into(), "src/café.rs".into());
        row.item.fields.insert("changes".into(), json!([{"operation":"update","path":"src/café.rs"},{"operation":"rename","oldPath":"old.rs","path":"new.rs"},{"operation":"delete","path":"gone.rs"}]));
        row.item.fields.insert("additions".into(), 0.into());
        let source = source(&row).unwrap();
        assert!(!source.omitted);
        assert_eq!(
            source.output.as_deref(),
            Some("update src/café.rs\nrename old.rs → new.rs\ndelete gone.rs\n\nLines: +0")
        );
        row.item.fields.remove("changes");
        row.item.fields.remove("additions");
        assert_eq!(output(&row.item).as_deref(), Some("src/café.rs"));
    }
    #[test]
    fn failed_edit_patches_remain_distinct_from_successful_file_changes() {
        let mut row = row();
        row.item.kind = "file_change".into();
        row.item
            .fields
            .insert("fileName".into(), "example.rs".into());
        row.item
            .fields
            .insert("diffStr".into(), "@@ -1 +1 @@\n-old\n+new".into());
        row.item.status = "completed".into();
        assert_eq!(output(&row.item).as_deref(), Some("example.rs"));
        row.item.status = "failed".into();
        assert_eq!(
            output(&row.item).as_deref(),
            Some("example.rs\n\nAttempted patch\n@@ -1 +1 @@\n-old\n+new")
        );
    }
}
