//! Image order matches toolOutputImages, whose index the asset endpoint uses.
use crate::thread::TurnItem;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub thread_id: String,
    pub item_id: String,
    pub index: usize,
    pub mime_type: String,
}
impl Image {
    pub fn resource(&self) -> Value {
        json!({"_tag":"tool-output-image","threadId":self.thread_id,"itemId":self.item_id,"index":self.index})
    }
    pub fn name(&self) -> String {
        format!(
            "image-{}.{}",
            self.index + 1,
            self.mime_type.strip_prefix("image/").unwrap_or("png")
        )
    }
}

fn mime(block: &Value) -> Option<String> {
    if block["type"] != "image" {
        return None;
    }
    let mime = if block["source"].is_object() {
        if block["source"]["type"] != "base64" {
            return None;
        }
        block["source"]["media_type"].as_str()?
    } else {
        block["mimeType"].as_str()?
    }
    .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
    .then_some(mime)
}

pub(crate) fn image_blocks(output: &Value) -> Vec<&Value> {
    let blocks = output
        .as_array()
        .or_else(|| output.get("content").and_then(Value::as_array));
    let blocks = blocks
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(output));
    blocks
        .iter()
        .filter(|block| mime(block).is_some())
        .take(8)
        .collect()
}

pub fn images(item: &TurnItem, thread_id: &str, item_id: &str) -> Vec<Image> {
    if item.kind != "dynamic_tool" || item.fields.get("outputOmitted") == Some(&Value::Bool(true)) {
        return Vec::new();
    }
    let Some(output) = item.fields.get("output") else {
        return Vec::new();
    };
    image_blocks(output)
        .into_iter()
        .filter_map(mime)
        .enumerate()
        .map(|(index, mime_type)| Image {
            thread_id: thread_id.into(),
            item_id: item_id.into(),
            index,
            mime_type,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(output: Value) -> TurnItem {
        serde_json::from_value(json!({"id":"visible","threadId":"child","type":"dynamic_tool","ordinal":0,"status":"completed","updatedAt":"now","input":{},"output":output})).unwrap()
    }
    #[test]
    fn recognized_images_keep_the_server_index_and_source_identity() {
        let item = item(
            json!({"content":[{"type":"text","text":"before"},{"type":"image","mimeType":"image/svg+xml"},{"type":"image","mimeType":"IMAGE/PNG"},{"type":"image","source":{"type":"url","media_type":"image/png"}},{"type":"image","source":{"type":"base64","media_type":"image/jpeg"}}]}),
        );
        let images = images(&item, "parent", "original");
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[1].name(), "image-2.jpeg");
        assert_eq!(
            images[1].resource(),
            json!({"_tag":"tool-output-image","threadId":"parent","itemId":"original","index":1})
        );
    }
    #[test]
    fn metadata_only_blocks_single_images_and_the_eight_image_cap_match_the_protocol() {
        let block = json!({"type":"image","mimeType":"image/webp"});
        assert_eq!(images(&item(block.clone()), "t", "i").len(), 1);
        assert_eq!(images(&item(json!(vec![block; 12])), "t", "i").len(), 8);
        let mut omitted = item(json!({"type":"image","mimeType":"image/png"}));
        omitted.fields.insert("outputOmitted".into(), true.into());
        assert!(images(&omitted, "t", "i").is_empty());
    }
    #[test]
    fn previews_replace_only_recognized_image_placeholders() {
        let block = json!({"type":"image","mimeType":"image/png"});
        let value = item(json!({"content":[{"type":"text","text":"[image]"},block.clone()]}));
        assert_eq!(
            crate::tool_detail::output(&value).as_deref(),
            Some("[image]")
        );
        let value = item(json!(vec![block; 9]));
        assert_eq!(
            crate::tool_detail::output(&value).as_deref(),
            Some("[image]")
        );
    }
}
