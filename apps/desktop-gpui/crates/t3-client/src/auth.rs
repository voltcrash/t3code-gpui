//! HTTP half of environment authentication.
//!
//! The socket never carries a long-lived token: a bearer token buys a short
//! WebSocket ticket over HTTP, and the ticket goes in the socket URL. See
//! `docs/internals/environment-auth.md`.

use serde::Deserialize;
use url::Url;

const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
/// `ORCHESTRATION_PROTOCOL_VERSION` in `packages/contracts/src/environment.ts`.
/// The server refuses sockets that declare any other version, so bump this
/// only together with the client code that speaks the new protocol.
pub const ORCHESTRATION_PROTOCOL_VERSION: &str = "2";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{operation} failed with HTTP {status}: {body}")]
    Status {
        operation: &'static str,
        status: u16,
        body: String,
    },
    #[error("{operation} failed: {source}")]
    Transport {
        operation: &'static str,
        #[source]
        source: reqwest::Error,
    },
}

impl AuthError {
    /// 502-504 mean a proxy or a backend that is still starting; worth retrying.
    pub fn is_transient(&self) -> bool {
        match self {
            AuthError::Status { status, .. } => matches!(status, 502..=504),
            AuthError::Transport { .. } => true,
        }
    }
}

/// How this client presents itself in the environment's connection list.
#[derive(Debug, Clone)]
pub struct ClientMetadata {
    pub label: String,
    pub app_version: String,
}

impl ClientMetadata {
    fn os() -> &'static str {
        match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            "linux" => "linux",
            _ => "unknown",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: f64,
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WebSocketTicket {
    ticket: String,
}

/// Exchanges a bootstrap or pairing credential for a bearer token.
pub async fn exchange_bootstrap_credential(
    http: &reqwest::Client,
    http_base: &Url,
    credential: &str,
    metadata: &ClientMetadata,
) -> Result<AccessToken, AuthError> {
    const OPERATION: &str = "token exchange";
    let form = [
        ("grant_type", TOKEN_EXCHANGE_GRANT),
        ("subject_token", credential),
        ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
        ("client_label", metadata.label.as_str()),
        ("client_device_type", "desktop"),
        ("client_os", ClientMetadata::os()),
    ];
    let response = http
        .post(join(http_base, "/oauth/token"))
        .form(&form)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            operation: OPERATION,
            source,
        })?;
    read_json(OPERATION, response).await
}

/// Builds an authenticated socket URL from a bearer token.
pub async fn websocket_url(
    http: &reqwest::Client,
    http_base: &Url,
    bearer_token: &str,
    metadata: &ClientMetadata,
) -> Result<Url, AuthError> {
    const OPERATION: &str = "WebSocket ticket";
    let response = http
        .post(join(http_base, "/api/auth/websocket-ticket"))
        .bearer_auth(bearer_token)
        .send()
        .await
        .map_err(|source| AuthError::Transport {
            operation: OPERATION,
            source,
        })?;
    let issued: WebSocketTicket = read_json(OPERATION, response).await?;

    let mut url = join(http_base, "/ws");
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .expect("http(s) URLs accept ws(s) schemes");
    url.query_pairs_mut()
        .append_pair("wsTicket", &issued.ticket)
        .append_pair("orchestrationProtocol", ORCHESTRATION_PROTOCOL_VERSION)
        .append_pair("clientSurface", "desktop")
        .append_pair("clientAppVersion", &metadata.app_version)
        .append_pair("clientDeviceType", "desktop")
        .append_pair("clientOs", ClientMetadata::os());
    Ok(url)
}

fn join(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(path);
    url.set_query(None);
    url
}

async fn read_json<T: serde::de::DeserializeOwned>(
    operation: &'static str,
    response: reqwest::Response,
) -> Result<T, AuthError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Status {
            operation,
            status: status.as_u16(),
            body,
        });
    }
    response
        .json()
        .await
        .map_err(|source| AuthError::Transport { operation, source })
}
