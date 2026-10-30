//! Provider status and settings, kept current by `subscribeServerConfig`.

use serde::Deserialize;
use serde_json::Value;

use crate::providers::{self, Provider, Settings};

pub const SUBSCRIBE: &str = "subscribeServerConfig";

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
    pub editors: crate::editors::Catalog,
    pub new_projects_root: Option<String>,
    pub providers_loaded: bool,
    /// `None` until a settings snapshot arrives.
    pub settings: Option<Settings>,
    pub thread_find: bool,
    pub thread_visited_tracking: bool,
    pub thread_find_progressive: bool,
}

impl ServerState {
    /// Applies one stream item. Returns whether providers or settings changed.
    pub fn apply(&mut self, item: ConfigStreamItem) -> bool {
        match item {
            ConfigStreamItem::Snapshot { config } => {
                self.thread_find = config["threadFind"] == true;
                self.thread_visited_tracking =
                    config["environment"]["capabilities"]["threadVisitedTracking"] == true;
                self.thread_find_progressive = config["threadFindProgressive"] == true;
                self.new_projects_root = config["newProjectsRoot"].as_str().map(str::to_owned);
                self.editors = crate::editors::Catalog::from_config(&config);
                self.providers = providers::from_config(&config);
                self.providers_loaded = true;
                self.settings = serde_json::from_value(config["settings"].clone()).ok();
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
        let mut state = ServerState::default();
        let snapshot = json!({ "version": 1, "type": "snapshot", "config": { "availableEditors":["zed"], "providers": [provider("codex")], "settings": { "defaultRuntimeMode": "full-access" } } });
        assert!(state.apply(serde_json::from_value(snapshot).unwrap()));
        assert_eq!(
            state
                .settings
                .as_ref()
                .unwrap()
                .default_runtime_mode
                .as_deref(),
            Some("full-access")
        );
        let update = json!({ "version": 1, "type": "providerStatuses", "payload": { "providers": [provider("claudeAgent")] } });
        assert!(state.apply(serde_json::from_value(update).unwrap()));
        assert_eq!(state.providers.len(), 1);
        assert_eq!(state.providers[0].instance_id, "claudeAgent");
        assert!(state.settings.is_some());
        assert_eq!(state.editors.preferred(None).unwrap().id, "zed");
        state.apply(ConfigStreamItem::Snapshot {
            config: json!({"availableEditors":["vscode"]}),
        });
        assert_eq!(state.editors.preferred(Some("zed")).unwrap().id, "vscode");
        state.apply(ConfigStreamItem::Snapshot { config: json!({}) });
        assert!(state.editors.available.is_empty());
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

    #[test]
    fn visit_tracking_requires_the_server_capability() {
        let mut state = ServerState::default();
        state.apply(ConfigStreamItem::Snapshot {
            config: json!({"environment":{"capabilities":{"threadVisitedTracking":true}}}),
        });
        assert!(state.thread_visited_tracking);
        state.apply(ConfigStreamItem::ProviderStatuses {
            payload: json!({"providers":[]}),
        });
        assert!(state.thread_visited_tracking);
        state.apply(ConfigStreamItem::Snapshot { config: json!({}) });
        assert!(!state.thread_visited_tracking);
    }

    #[test]
    fn thread_find_follows_server_capabilities_and_survives_provider_updates() {
        let mut state = ServerState::default();
        state.apply(ConfigStreamItem::Snapshot {
            config: json!({ "threadFind": true, "threadFindProgressive": true }),
        });
        assert!(state.thread_find && state.thread_find_progressive);
        state.apply(ConfigStreamItem::ProviderStatuses {
            payload: json!({ "providers": [] }),
        });
        assert!(state.thread_find && state.thread_find_progressive);
        state.apply(ConfigStreamItem::Snapshot { config: json!({}) });
        assert!(!state.thread_find && !state.thread_find_progressive);
    }
}
