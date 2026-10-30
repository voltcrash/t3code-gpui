//! Provider settings mutations use the existing per-instance upsert, so an
//! unrelated account changed by another client is never sent back as a stale map.

use crate::providers::{Provider, Settings};
use serde_json::{Map, Value, json};

pub const UPDATE: &str = "server.updateSettings";

pub fn set_enabled(settings: &Settings, provider: &Provider, enabled: bool) -> Option<Value> {
    let mut instance = match settings.provider_instances.get(&provider.instance_id) {
        Some(value) => value.as_object()?.clone(),
        None => Map::from_iter([("driver".into(), json!(provider.driver))]),
    };
    if instance.get("driver")?.as_str()? != provider.driver {
        return None;
    }
    instance.insert("enabled".into(), json!(enabled));
    // Older driver configs also carry an enablement flag. Both must agree;
    // resolveProviderInstanceEnabled treats either false flag as disabled.
    if let Some(Value::Bool(flag)) = instance
        .get_mut("config")
        .and_then(Value::as_object_mut)
        .and_then(|config| config.get_mut("enabled"))
    {
        *flag = enabled;
    }
    Some(json!({
        "patch": {},
        "providerInstanceMutation": {
            "operation": "upsert", "instanceId": provider.instance_id, "instance": instance,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider(id: &str, driver: &str) -> Provider {
        serde_json::from_value(json!({
            "instanceId": id, "driver": driver, "enabled": false, "status": "disabled",
        }))
        .unwrap()
    }

    #[test]
    fn toggles_only_the_selected_instance_and_preserves_opaque_config_and_redactions() {
        let instance = json!({
            "driver":"codex", "displayName":"Work", "accentColor":"#abc123", "enabled":false,
            "environment":[{"name":"TOKEN", "value":"", "sensitive":true, "valueRedacted":true}],
            "config":{"binaryPath":"custom codex", "futureField":{"nested":42}},
        });
        let settings: Settings = serde_json::from_value(json!({"providerInstances":{
            "work":instance, "another":{"driver":"codex", "enabled":true},
        }}))
        .unwrap();
        let request = set_enabled(&settings, &provider("work", "codex"), true).unwrap();
        assert_eq!(request["patch"], json!({}));
        assert!(request.get("providerInstances").is_none());
        assert_eq!(request["providerInstanceMutation"]["operation"], "upsert");
        assert_eq!(request["providerInstanceMutation"]["instanceId"], "work");
        let mut expected = instance.clone();
        expected["enabled"] = json!(true);
        assert_eq!(request["providerInstanceMutation"]["instance"], expected);
        assert_eq!(settings.provider_instances["work"], instance);
    }

    #[test]
    fn legacy_flags_can_be_enabled_and_disabled_without_losing_other_driver_settings() {
        let mut settings: Settings = serde_json::from_value(json!({"providerInstances":{
            "codex":{"driver":"codex", "enabled":false, "config":{"enabled":false,"binaryPath":"custom"}},
        }})).unwrap();
        for enabled in [true, false] {
            let request = set_enabled(&settings, &provider("codex", "codex"), enabled).unwrap();
            let instance = request["providerInstanceMutation"]["instance"].clone();
            assert_eq!(instance["enabled"], enabled);
            assert_eq!(instance["config"]["enabled"], enabled);
            assert_eq!(instance["config"]["binaryPath"], "custom");
            settings.provider_instances.insert("codex".into(), instance);
            assert_eq!(
                settings.instance_enabled(None, &provider("codex", "codex")),
                enabled
            );
        }
    }

    #[test]
    fn materializes_an_implicit_instance_but_rejects_stale_or_unreadable_settings() {
        let codex = provider("codex", "codex");
        let request = set_enabled(&Settings::default(), &codex, false).unwrap();
        assert_eq!(
            request["providerInstanceMutation"]["instance"],
            json!({"driver":"codex","enabled":false})
        );
        for value in [
            json!("unreadable"),
            json!({"driver":"claudeAgent","enabled":true}),
        ] {
            let settings: Settings =
                serde_json::from_value(json!({"providerInstances":{"codex":value}})).unwrap();
            assert!(set_enabled(&settings, &codex, true).is_none());
        }
    }
}
