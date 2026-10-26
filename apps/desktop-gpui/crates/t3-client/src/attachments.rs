//! Signed uploads and asset reads use the unchanged environment protocol.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::{RpcClient, RpcError};

pub const MAX_COUNT: usize = 100;
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: u64 = 80 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

pub fn metadata(name: &str, size_bytes: u64) -> Result<Metadata, Error> {
    let name = name.trim();
    if name.is_empty() || name.encode_utf16().count() > 255 {
        return Err(Error::Invalid(
            "Choose a file with a name of 1–255 characters.",
        ));
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "rs" | "ts" | "js" | "json" | "log" | "csv" => "text/plain",
        _ => "application/octet-stream",
    };
    let image = mime.starts_with("image/");
    if size_bytes == 0 {
        return Err(Error::Invalid("Empty files cannot be attached."));
    }
    if size_bytes
        > if image {
            MAX_IMAGE_BYTES
        } else {
            MAX_FILE_BYTES
        }
    {
        return Err(Error::Invalid(if image {
            "Images must be at most 10 MiB."
        } else {
            "Files must be at most 50 MiB."
        }));
    }
    Ok(Metadata {
        kind: if image { "image" } else { "file" },
        name: name.into(),
        mime_type: mime.into(),
        size_bytes,
    })
}

pub fn read_file(path: &Path) -> Result<(Metadata, Vec<u8>), Error> {
    let stat = std::fs::metadata(path)?;
    if !stat.is_file() {
        return Err(Error::Invalid("Choose files rather than folders."));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::Invalid("This file name cannot be attached."))?;
    let initial = metadata(name, stat.len())?;
    let file = std::fs::File::open(path)?;
    let limit = if initial.kind == "image" {
        MAX_IMAGE_BYTES
    } else {
        MAX_FILE_BYTES
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    let metadata = metadata(name, bytes.len() as u64)?;
    Ok((metadata, bytes))
}

pub fn validate_batch<'a>(
    attachments: impl IntoIterator<Item = &'a Metadata>,
) -> Result<(), Error> {
    let mut count = 0;
    let mut image_bytes = 0;
    for attachment in attachments {
        count += 1;
        if attachment.kind == "image" {
            image_bytes += attachment.size_bytes;
        }
    }
    if count > MAX_COUNT {
        return Err(Error::Invalid(
            "You can attach up to 100 files per message.",
        ));
    }
    if image_bytes > MAX_TOTAL_IMAGE_BYTES {
        return Err(Error::Invalid(
            "Images can total at most 80 MiB per message.",
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Could not read this file: {0}")]
    File(#[from] std::io::Error),
    #[error("{0}")]
    Rpc(#[from] RpcError),
    #[error("Attachment transfer failed. Check the connection and retry.")]
    Transport(#[from] reqwest::Error),
    #[error("Attachment transfer failed (HTTP {0}). Retry or remove the attachment.")]
    Status(u16),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    pending: Arc<Mutex<Vec<Weak<Pending>>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadUrl {
    attachment_id: String,
    relative_url: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetUrl {
    relative_url: String,
}

/// The last draft owner discards its pending upload. A send keeps an owner until
/// the server accepts the reference, even if the composer disappears meanwhile.
pub struct Pending {
    pub attachment: Attachment,
    rpc: Mutex<RpcClient>,
    runtime: tokio::runtime::Handle,
    sent: AtomicBool,
}
impl Pending {
    pub fn mark_sent(&self) {
        // The server copies claimed files into the thread; the pending retry source can go.
        self.cleanup();
    }
    fn cleanup(&self) {
        if self.sent.swap(true, Ordering::AcqRel) {
            return;
        }
        let rpc = self.rpc.lock().expect("upload RPC lock").clone();
        let id = self.attachment.id.clone();
        self.runtime.spawn(async move {
            let _ = rpc
                .call::<_, Value>("attachments.delete", &json!({"attachmentId":id}))
                .await;
        });
    }
    pub async fn discard(&self) -> Result<(), RpcError> {
        let rpc = self.rpc.lock().expect("upload RPC lock").clone();
        let result = rpc
            .call::<_, Value>(
                "attachments.delete",
                &json!({"attachmentId":self.attachment.id}),
            )
            .await;
        if result.is_ok() {
            self.sent.store(true, Ordering::Release);
        }
        result.map(|_| ())
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl Client {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self {
            http,
            base,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }
    pub fn reconnected(&self, rpc: &RpcClient) {
        for pending in self
            .pending
            .lock()
            .expect("pending uploads lock")
            .iter()
            .filter_map(Weak::upgrade)
        {
            *pending.rpc.lock().expect("upload RPC lock") = rpc.clone();
        }
    }
    fn url(&self, relative: &str) -> Result<Url, Error> {
        let url = self
            .base
            .join(relative)
            .map_err(|_| Error::Invalid("The server returned an invalid attachment URL."))?;
        if url.origin() != self.base.origin() {
            return Err(Error::Invalid(
                "The server returned an attachment URL for another environment.",
            ));
        }
        Ok(url)
    }
    pub async fn upload(
        &self,
        rpc: &RpcClient,
        metadata: Metadata,
        bytes: Vec<u8>,
    ) -> Result<Arc<Pending>, Error> {
        let url: UploadUrl = rpc.call("attachments.createUploadUrl", &metadata).await?;
        let pending = Arc::new(Pending {
            attachment: Attachment {
                kind: metadata.kind.into(),
                id: url.attachment_id,
                name: metadata.name,
                mime_type: metadata.mime_type,
                size_bytes: metadata.size_bytes,
                source: None,
            },
            rpc: Mutex::new(rpc.clone()),
            runtime: tokio::runtime::Handle::current(),
            sent: AtomicBool::new(false),
        });
        let response = self
            .http
            .post(self.url(&url.relative_url)?)
            .header("content-type", &pending.attachment.mime_type)
            .timeout(Duration::from_secs(120))
            .body(bytes)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        let mut tracked = self.pending.lock().expect("pending uploads lock");
        tracked.retain(|upload| upload.strong_count() > 0);
        tracked.push(Arc::downgrade(&pending));
        Ok(pending)
    }
    pub async fn discard_pending(&self) {
        let pending = self
            .pending
            .lock()
            .expect("pending uploads lock")
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        futures_util::future::join_all(
            pending
                .iter()
                .filter(|upload| !upload.sent.load(Ordering::Acquire))
                .map(|upload| upload.discard()),
        )
        .await;
    }
    pub async fn download(
        &self,
        rpc: &RpcClient,
        attachment: &Attachment,
    ) -> Result<Vec<u8>, Error> {
        let asset: AssetUrl = rpc.call("assets.createUrl", &json!({"resource":{
            "_tag":"attachment", "attachmentId":attachment.id, "fileName":attachment.name, "mimeType":attachment.mime_type
        }})).await?;
        let mut response = self
            .http
            .get(self.url(&asset.relative_url)?)
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Status(response.status().as_u16()));
        }
        let limit = if attachment.kind == "image" {
            MAX_IMAGE_BYTES
        } else {
            MAX_FILE_BYTES
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() as u64 + chunk.len() as u64 > limit {
                return Err(Error::Invalid(
                    "This attachment exceeds the download limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_limits_and_image_classification_follow_the_contract() {
        assert_eq!(
            metadata("Screenshot.PNG", MAX_IMAGE_BYTES).unwrap().kind,
            "image"
        );
        assert!(metadata("Screenshot.png", MAX_IMAGE_BYTES + 1).is_err());
        assert_eq!(metadata("diagram.svg", 40).unwrap().kind, "file");
        assert!(metadata("empty.txt", 0).is_err());
        assert!(metadata("archive.zip", MAX_FILE_BYTES + 1).is_err());
        let image = metadata("image.png", MAX_IMAGE_BYTES).unwrap();
        assert!(validate_batch(std::iter::repeat_n(&image, 8)).is_ok());
        assert!(validate_batch(std::iter::repeat_n(&image, 9)).is_err());
        let file = metadata("file.txt", 10).unwrap();
        assert!(validate_batch(std::iter::repeat_n(&file, 100)).is_ok());
        assert!(validate_batch(std::iter::repeat_n(&file, 101)).is_err());
    }
    #[test]
    fn signed_paths_resolve_against_the_destination_environment() {
        let client = Client::new(
            reqwest::Client::new(),
            Url::parse("https://remote.example/t3/").unwrap(),
        );
        assert_eq!(
            client
                .url("/api/attachments/upload/signed")
                .unwrap()
                .as_str(),
            "https://remote.example/api/attachments/upload/signed"
        );
        assert!(client.url("https://other.example/upload").is_err());
    }
}
