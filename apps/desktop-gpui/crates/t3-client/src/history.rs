//! Authenticated, bounded history reads from the existing environment HTTP API.

use std::time::Duration;

use url::Url;

use crate::auth::ORCHESTRATION_PROTOCOL_VERSION;
use crate::thread::HistoryPage;

#[derive(Clone)]
pub struct HistoryClient {
    http: reqwest::Client,
    base: Url,
    access_token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("Could not load earlier messages. Check the connection and try again.")]
    Transport(#[from] reqwest::Error),
    #[error("Could not load earlier messages (HTTP {0}).")]
    Status(u16),
}

impl HistoryClient {
    pub fn new(http: reqwest::Client, base: Url, access_token: String) -> Self {
        Self {
            http,
            base,
            access_token,
        }
    }

    pub async fn page(&self, thread_id: &str, cursor: &str) -> Result<HistoryPage, HistoryError> {
        self.page_through(thread_id, cursor, None).await
    }

    pub async fn page_through(
        &self,
        thread_id: &str,
        cursor: &str,
        through_entry_id: Option<&str>,
    ) -> Result<HistoryPage, HistoryError> {
        let mut url = history_url(&self.base, thread_id, cursor);
        if let Some(entry) = through_entry_id {
            url.query_pairs_mut().append_pair("throughEntryId", entry);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.access_token)
            .header(
                "x-t3-orchestration-protocol",
                ORCHESTRATION_PROTOCOL_VERSION,
            )
            .timeout(Duration::from_secs(6))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(HistoryError::Status(response.status().as_u16()));
        }
        Ok(response.json().await?)
    }
}

fn history_url(base: &Url, thread_id: &str, cursor: &str) -> Url {
    let mut url = base.clone();
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .expect("environment URLs are HTTP(S)")
        .pop_if_empty()
        .extend(["api", "orchestration", "threads", thread_id, "history"]);
    // Cursors belong to the server. Encoding them as query values preserves their bytes.
    url.query_pairs_mut().append_pair("cursor", cursor);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_url_preserves_opaque_cursor_and_thread_identity() {
        let base = Url::parse("https://remote.example:8443/stale?token=unused#fragment").unwrap();
        let cursor = "opaque+/=?& cursor";
        let url = history_url(&base, "thread/with spaces?", cursor);
        assert_eq!(url.origin(), base.origin());
        assert_eq!(
            url.path(),
            "/api/orchestration/threads/thread%2Fwith%20spaces%3F/history"
        );
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [("cursor".into(), cursor.into())]
        );
        assert!(url.fragment().is_none());
    }
}
