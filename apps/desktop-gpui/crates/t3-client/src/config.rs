//! Provider status and settings, kept current by `subscribeServerConfig`.
//!
//! Its snapshot carries the providers but not the settings, so a client reads
//! `server.getSettings` once and then follows `settingsUpdated`.

use serde::Deserialize;
use serde_json::Value;

use crate::providers::{self, Provider, Settings};

pub const SUBSCRIBE: &str = "subscribeServerConfig";
pub const GET_SETTINGS: &str = "server.getSettings";

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ConfigStreamItem {
    #[serde(rename = "snapshot")]
    Snapshot { config: Value },
    #[serde(rename = "providerStatuses")]
    ProviderStatuses { payload: Value },
    #[serde(rename = "settingsUpdated")]
    SettingsUpdated { payload: Value },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default)]
pub struct ServerState {
    pub providers: Vec<Provider>,
    pub providers_loaded: bool,
    /// `None` until `server.getSettings` answers.
    pub settings: Option<Settings>,
}

impl ServerState {
    /// Applies one stream item. Returns whether providers or settings changed.
    pub fn apply(&mut self, item: ConfigStreamItem) -> bool {
        match item {
            ConfigStreamItem::Snapshot { config } => {
                self.providers = providers::from_config(&config);
                self.providers_loaded = true;
                true
            }
            ConfigStreamItem::ProviderStatuses { payload } => {
                self.providers = providers::from_config(&payload);
                self.providers_loaded = true;
                true
            }
            ConfigStreamItem::SettingsUpdated { payload } => {
                match serde_json::from_value(payload["settings"].clone()) {
                    Ok(settings) => {
                        self.settings = Some(settings);
                        true
                    }
                    Err(error) => {
                        tracing::warn!("unreadable settings update: {error}");
                        false
                    }
                }
            }
            ConfigStreamItem::Other => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(id: &str) -> Value {
        json!({ "instanceId": id, "driver": id, "enabled": true, "status": "ready", "models": [] })
    }

    #[test]
    fn provider_updates_replace_the_list_and_keep_settings() {
        let mut state = ServerState {
            settings: Some(Settings::default()),
            ..Default::default()
        };
        let snapshot = json!({ "version": 1, "type": "snapshot", "config": { "providers": [provider("codex")] } });
        assert!(state.apply(serde_json::from_value(snapshot).unwrap()));
        let update = json!({ "version": 1, "type": "providerStatuses", "payload": { "providers": [provider("claudeAgent")] } });
        assert!(state.apply(serde_json::from_value(update).unwrap()));
        assert_eq!(state.providers.len(), 1);
        assert_eq!(state.providers[0].instance_id, "claudeAgent");
        assert!(state.settings.is_some());
    }

    #[test]
    fn settings_updates_and_unknown_events() {
        let mut state = ServerState::default();
        let update = json!({ "version": 1, "type": "settingsUpdated", "payload": { "settings": { "defaultRuntimeMode": "auto" } } });
        assert!(state.apply(serde_json::from_value(update).unwrap()));
        assert_eq!(
            state.settings.unwrap().default_runtime_mode.as_deref(),
            Some("auto")
        );
        let other = json!({ "version": 1, "type": "keybindingsUpdated", "payload": {} });
        assert!(!ServerState::default().apply(serde_json::from_value(other).unwrap()));
    }
}
