//! Provider instances from `server.getConfig` and the model a new thread
//! starts with when the person has not picked one.
//!
//! Ports `apps/web/src/providerInstances.ts`, `resolveComposerProviderSelection`
//! in `ChatView.logic.ts`, the project override rules of
//! `packages/shared/src/projectSettings.ts`, and
//! `resolveProjectProviderInstanceEnabled` in `packages/contracts/src/settings.ts`.
//! Keep them in step.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// `RuntimeMode`, in the order the web offers them.
const RUNTIME_MODES: [&str; 4] = [
    "approval-required",
    "auto-accept-edits",
    "auto",
    "full-access",
];
pub const DEFAULT_RUNTIME_MODE: &str = "full-access";
/// Matches the `enabled` decoding default of each driver's settings schema.
const DEFAULT_DISABLED_DRIVERS: [&str; 6] =
    ["cursor", "grok", "muse", "pi", "opencode", "antigravity"];
const DEFAULT_ENABLED_INSTANCES: [&str; 2] = ["codex", "claudeAgent"];

/// `DEFAULT_MODEL_BY_PROVIDER`.
fn default_model_for_driver(driver: &str) -> Option<&'static str> {
    Some(match driver {
        "codex" => "gpt-6-astra",
        "claudeAgent" => "claude-fable-5-1",
        "cursor" => "auto",
        "grok" => "grok-build",
        "muse" | "acpRegistry" | "pi" => "default",
        "opencode" => "openai/gpt-5",
        "antigravity" => "antigravity-default",
        _ => return None,
    })
}

/// `PROVIDER_DISPLAY_NAMES`.
fn driver_display_name(driver: &str) -> Option<&'static str> {
    Some(match driver {
        "antigravity" => "Antigravity",
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "muse" => "Muse Code",
        "acpRegistry" => "ACP Registry",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        _ => return None,
    })
}

fn humanize_slug(slug: &str) -> String {
    let mut spaced = String::new();
    let mut previous_lower = false;
    for ch in slug.chars() {
        if ch.is_ascii_uppercase() && previous_lower {
            spaced.push(' ');
        }
        previous_lower = ch.is_ascii_lowercase();
        spaced.push(if ch == '_' || ch == '-' { ' ' } else { ch });
    }
    spaced
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub is_custom: bool,
    #[serde(default)]
    pub is_default: bool,
}

/// A subset of `ServerProvider`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub instance_id: String,
    pub driver: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub status: String,
    pub availability: Option<String>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    pub supported_runtime_modes: Option<Vec<String>>,
    pub show_interaction_mode_toggle: Option<bool>,
    #[serde(default)]
    pub requires_new_thread_for_model_change: bool,
}

impl Provider {
    /// `resolveProviderInstanceDisplayName`.
    pub fn display_name(&self) -> String {
        let kind = driver_display_name(&self.driver)
            .map(str::to_owned)
            .unwrap_or_else(|| humanize_slug(&self.driver));
        let named = self
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if let Some(name) = named.filter(|name| *name != kind) {
            return name.to_owned();
        }
        if self.instance_id != self.driver {
            let humanized = humanize_slug(&self.instance_id);
            if !humanized.is_empty() {
                return humanized;
            }
        }
        named.map(str::to_owned).unwrap_or(kind)
    }

    fn is_default_instance(&self) -> bool {
        self.instance_id == self.driver
    }

    /// `getDefaultProviderInstanceModel`.
    fn default_model(&self) -> Option<String> {
        self.models
            .iter()
            .find(|model| model.is_default && !model.is_custom)
            .or_else(|| self.models.iter().find(|model| !model.is_custom))
            .or_else(|| self.models.first())
            .map(|model| model.slug.clone())
            .or_else(|| default_model_for_driver(&self.driver).map(str::to_owned))
    }

    pub fn model_name(&self, slug: &str) -> String {
        self.models
            .iter()
            .find(|model| model.slug == slug)
            .map_or_else(|| slug.to_owned(), |model| model.name.clone())
    }
}

/// Reads `providers` from a `server.getConfig` result; unreadable entries are skipped.
pub fn from_config(config: &Value) -> Vec<Provider> {
    config["providers"]
        .as_array()
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| serde_json::from_value(provider.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The parts of `ServerSettings` that decide a new thread's defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub provider_instances: Map<String, Value>,
    #[serde(default)]
    pub provider_model_preferences: Option<Map<String, Value>>,
    #[serde(default)]
    pub project_settings_overrides: Map<String, Value>,
    pub default_model_selection: Option<Value>,
    pub default_runtime_mode: Option<String>,
    pub default_thread_env_mode: Option<String>,
    #[serde(default)]
    pub project_settings_folded: bool,
}

impl Settings {
    /// `resolveProjectProviderInstanceEnabled`.
    pub fn instance_enabled(&self, project_id: Option<&str>, provider: &Provider) -> bool {
        let id = provider.instance_id.as_str();
        let project_override = project_id
            .and_then(|project| self.project_settings_overrides.get(project))
            .and_then(|overrides| overrides["providerInstances"][id]["enabled"].as_bool());
        if let Some(enabled) = project_override {
            return enabled;
        }
        match self.provider_instances.get(id) {
            // `resolveProviderInstanceEnabled`.
            Some(instance) => {
                let config_enabled = instance["config"]["enabled"].as_bool();
                let enabled = instance["enabled"].as_bool();
                if enabled == Some(false) || config_enabled == Some(false) {
                    return false;
                }
                let driver = instance["driver"].as_str().unwrap_or(&provider.driver);
                enabled
                    .or(config_enabled)
                    .unwrap_or(!DEFAULT_DISABLED_DRIVERS.contains(&driver))
            }
            None => DEFAULT_ENABLED_INSTANCES.contains(&id),
        }
    }

    /// A project-scoped setting: the project's override, then the
    /// environment value. Before the server folds legacy project fields into
    /// the override record, those fields sit underneath it.
    fn project_value(&self, project: Option<&ProjectDefaults>, key: &str) -> Option<Value> {
        let stored = project
            .and_then(|project| self.project_settings_overrides.get(&project.id))
            .and_then(|overrides| overrides.get(key))
            .filter(|value| !value.is_null())
            .cloned();
        let legacy = (!self.project_settings_folded)
            .then(|| project.and_then(|project| project.legacy(key)))
            .flatten();
        stored.or(legacy)
    }
}

/// A project's id and the legacy defaults its shell record still carries.
#[derive(Debug, Clone, Default)]
pub struct ProjectDefaults {
    pub id: String,
    pub default_model_selection: Option<Value>,
}

impl ProjectDefaults {
    fn legacy(&self, key: &str) -> Option<Value> {
        (key == "defaultModelSelection")
            .then(|| self.default_model_selection.clone())
            .flatten()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewThreadDefaults {
    pub model_selection: Value,
    pub provider_label: String,
    pub model_label: String,
    pub runtime_mode: String,
}

/// The provider, model and runtime mode a new thread in `project` starts with:
/// the project's then the environment's default model while its instance is
/// usable, otherwise the first ready instance and its default model. `None`
/// when no provider can run.
pub fn new_thread_defaults(
    providers: &[Provider],
    settings: &Settings,
    project: &ProjectDefaults,
) -> Option<NewThreadDefaults> {
    // `applyProviderInstanceSettings` then `sortProviderInstanceEntries`.
    let entries: Vec<(&Provider, bool)> = sorted(providers)
        .into_iter()
        .map(|provider| {
            (
                provider,
                settings.instance_enabled(Some(&project.id), provider),
            )
        })
        .collect();

    // A project default on an instance the project disabled does not count.
    let project_selection = settings
        .project_value(Some(project), "defaultModelSelection")
        .filter(|selection| {
            let instance = selection["instanceId"].as_str();
            entries.iter().any(|(provider, enabled)| {
                Some(provider.instance_id.as_str()) == instance && *enabled
            })
        });
    let selection = project_selection.or_else(|| settings.default_model_selection.clone());
    let requested = selection
        .as_ref()
        .and_then(|selection| selection["instanceId"].as_str())
        .filter(|id| *id != "t3code_no_provider");

    let requested_entry = requested.and_then(|id| {
        entries
            .iter()
            .find(|entry| entry.0.instance_id == id && selectable(entry))
    });
    let requested_kind = requested
        .and_then(|id| entries.iter().find(|entry| entry.0.instance_id == id))
        .or(entries.first())
        .map(|entry| entry.0.driver.clone());
    let (provider, _) = requested_entry.copied().or_else(|| {
        first_selectable(
            entries
                .iter()
                .filter(|entry| Some(&entry.0.driver) == requested_kind.as_ref()),
        )
        .or_else(|| first_selectable(entries.iter()))
    })?;

    let same_instance = selection
        .as_ref()
        .and_then(|selection| selection["instanceId"].as_str())
        == Some(provider.instance_id.as_str());
    let chosen_model = selection
        .as_ref()
        .filter(|_| same_instance)
        .and_then(|selection| selection["model"].as_str())
        .filter(|model| !model.is_empty())
        .map(str::to_owned);
    let model = chosen_model.or_else(|| provider.default_model())?;
    let mut model_selection = json!({ "instanceId": provider.instance_id, "model": model });
    if let Some(options) = selection
        .as_ref()
        .filter(|_| same_instance)
        .and_then(|selection| selection.get("options"))
        .filter(|options| {
            options
                .as_array()
                .is_some_and(|options| !options.is_empty())
        })
    {
        model_selection["options"] = options.clone();
    }

    let requested_mode = settings
        .project_value(Some(project), "defaultRuntimeMode")
        .and_then(|mode| mode.as_str().map(str::to_owned))
        .or_else(|| settings.default_runtime_mode.clone())
        .unwrap_or_else(|| DEFAULT_RUNTIME_MODE.to_owned());
    let runtime_mode = match provider
        .supported_runtime_modes
        .as_ref()
        .filter(|modes| !modes.is_empty())
    {
        Some(supported) if !supported.contains(&requested_mode) => RUNTIME_MODES
            .iter()
            .find(|mode| supported.iter().any(|supported| supported == *mode))
            .map_or(requested_mode, |mode| (*mode).to_owned()),
        _ => requested_mode,
    };

    Some(NewThreadDefaults {
        model_label: provider.model_name(&model),
        provider_label: provider.display_name(),
        model_selection,
        runtime_mode,
    })
}

fn selectable((provider, enabled): &(&Provider, bool)) -> bool {
    *enabled && provider.availability.as_deref() != Some("unavailable")
}

/// `resolveSelectableProviderInstanceEntry`: ready first, then not failing.
fn first_selectable<'a>(
    candidates: impl Iterator<Item = &'a (&'a Provider, bool)> + Clone,
) -> Option<(&'a Provider, bool)> {
    candidates
        .clone()
        .find(|entry| selectable(entry) && entry.0.status == "ready")
        .or_else(|| {
            candidates
                .clone()
                .find(|entry| selectable(entry) && entry.0.status != "error")
        })
        .copied()
}

/// `runtimeModeConfig` in `apps/web/src/components/chat/runtimeModeConfig.ts`.
pub fn runtime_mode_label(mode: &str) -> (&'static str, &'static str) {
    match mode {
        "approval-required" => ("Supervised", "Ask before commands and file changes."),
        "auto-accept-edits" => (
            "Auto-accept edits",
            "Auto-approve edits, ask before other actions.",
        ),
        "auto" => (
            "Auto",
            "Supported providers approve routine actions; others still ask.",
        ),
        _ => ("Full access", "Allow commands and edits without prompts."),
    }
}

/// The runtime modes a provider offers, in the web's order. A provider that
/// lists none offers them all.
pub fn runtime_modes_for(provider: Option<&Provider>) -> Vec<&'static str> {
    let supported = provider
        .and_then(|provider| provider.supported_runtime_modes.as_ref())
        .filter(|modes| !modes.is_empty());
    RUNTIME_MODES
        .iter()
        .copied()
        .filter(|mode| supported.is_none_or(|supported| supported.iter().any(|s| s == mode)))
        .collect()
}

/// A provider switch can invalidate the pending mode. Use the same first
/// supported mode as `compatibleRuntimeMode` in `ChatComposer.tsx`.
pub fn resolve_runtime_mode(provider: Option<&Provider>, requested: &str) -> String {
    let modes = runtime_modes_for(provider);
    if modes.contains(&requested) {
        requested.to_owned()
    } else {
        modes.first().copied().unwrap_or(requested).to_owned()
    }
}

/// Clicking the current model must not discard its configured options.
pub fn select_model(current: &Value, instance_id: &str, model: &str) -> Value {
    if current["instanceId"] == instance_id && current["model"] == model {
        current.clone()
    } else {
        json!({ "instanceId": instance_id, "model": model })
    }
}

/// `getAppModelOptionsForInstance`: rebuild custom rows from settings so a
/// stale provider probe cannot resurrect a removed model.
pub fn picker_models(provider: &Provider, settings: &Settings) -> Vec<ProviderModel> {
    let mut models: Vec<_> = provider
        .models
        .iter()
        .filter(|model| !model.is_custom)
        .cloned()
        .collect();
    let mut seen: HashSet<_> = models.iter().map(|model| model.slug.clone()).collect();
    if provider.driver != "antigravity"
        && let Some(custom) = settings
            .provider_instances
            .get(&provider.instance_id)
            .and_then(|instance| instance["config"]["customModels"].as_array())
    {
        for entry in custom {
            let slug = entry
                .as_str()
                .or_else(|| entry["slug"].as_str())
                .unwrap_or_default()
                .trim();
            if slug.is_empty() || slug.encode_utf16().count() > 256 || !seen.insert(slug.to_owned())
            {
                continue;
            }
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(slug);
            models.push(ProviderModel {
                slug: slug.to_owned(),
                name: name.to_owned(),
                is_custom: true,
                is_default: false,
            });
        }
    }
    if let Some(preferences) = settings
        .provider_model_preferences
        .as_ref()
        .and_then(|preferences| preferences.get(&provider.instance_id))
    {
        if let Some(hidden) = preferences["hiddenModels"].as_array() {
            models
                .retain(|model| model.is_custom || !hidden.iter().any(|slug| slug == &model.slug));
        }
        if let Some(order) = preferences["modelOrder"].as_array() {
            models.sort_by_key(|model| {
                order
                    .iter()
                    .rposition(|slug| slug == &model.slug)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    models
}

/// One instance in the model picker.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerProvider {
    pub instance_id: String,
    pub label: String,
    /// Enabled but not ready: listed, not choosable
    /// (`isProviderInstancePickerReady`).
    pub ready: bool,
    pub models: Vec<ProviderModel>,
}

/// The instances the picker lists for `project`: every enabled one, sorted
/// like the composer. A started thread stays on its driver
/// (`deriveLockedProvider`); switching providers needs a handoff this client
/// does not do yet.
pub fn picker_providers(
    providers: &[Provider],
    settings: &Settings,
    project_id: Option<&str>,
    locked_driver: Option<&str>,
) -> Vec<PickerProvider> {
    sorted(providers)
        .into_iter()
        .filter(|provider| locked_driver.is_none_or(|driver| provider.driver == driver))
        .filter(|provider| settings.instance_enabled(project_id, provider))
        .map(|provider| PickerProvider {
            instance_id: provider.instance_id.clone(),
            label: provider.display_name(),
            ready: provider.availability.as_deref() != Some("unavailable")
                && provider.status == "ready",
            models: picker_models(provider, settings),
        })
        .collect()
}

/// `sortProviderInstanceEntries`: drivers in first-seen order, each driver's
/// default instance first.
fn sorted(providers: &[Provider]) -> Vec<&Provider> {
    let mut kinds: Vec<&str> = Vec::new();
    for provider in providers {
        if !kinds.contains(&provider.driver.as_str()) {
            kinds.push(&provider.driver);
        }
    }
    kinds
        .iter()
        .flat_map(|kind| {
            let bucket = providers.iter().filter(move |p| p.driver == *kind);
            bucket
                .clone()
                .filter(|p| p.is_default_instance())
                .chain(bucket.filter(|p| !p.is_default_instance()))
        })
        .collect()
}

/// `getStartedThreadModelChangeBlockReason`, without provider handoff.
pub fn model_change_block(
    providers: &[Provider],
    started: bool,
    current: &Value,
    next: &Value,
) -> Option<&'static str> {
    if !started {
        return None;
    }
    let instance = |selection: &Value| selection["instanceId"].as_str().map(str::to_owned);
    if instance(current) == instance(next) && current["model"] == next["model"] {
        return None;
    }
    if instance(current) != instance(next) {
        return Some("Start a new chat to switch providers");
    }
    let requires_new_thread = |selection: &Value| {
        providers.iter().any(|provider| {
            Some(provider.instance_id.as_str()) == selection["instanceId"].as_str()
                && provider.requires_new_thread_for_model_change
        })
    };
    (requires_new_thread(current) || requires_new_thread(next))
        .then_some("Start a new chat to change models")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(instance: &str, driver: &str, status: &str, models: &[(&str, bool)]) -> Provider {
        serde_json::from_value(json!({
            "instanceId": instance,
            "driver": driver,
            "enabled": true,
            "status": status,
            "models": models.iter().map(|(slug, default)| json!({
                "slug": slug, "name": slug.to_uppercase(), "isCustom": false, "isDefault": default,
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn settings(value: Value) -> Settings {
        serde_json::from_value(value).unwrap()
    }

    fn project() -> ProjectDefaults {
        ProjectDefaults {
            id: "p".into(),
            default_model_selection: None,
        }
    }

    #[test]
    fn without_a_default_the_first_ready_instance_and_its_default_model_win() {
        let providers = [
            provider("codex", "codex", "error", &[("gpt", true)]),
            provider(
                "claudeAgent",
                "claudeAgent",
                "ready",
                &[("haiku", false), ("opus", true)],
            ),
        ];
        let defaults = new_thread_defaults(&providers, &settings(json!({})), &project()).unwrap();
        assert_eq!(
            defaults.model_selection,
            json!({ "instanceId": "claudeAgent", "model": "opus" })
        );
        assert_eq!(defaults.provider_label, "Claude");
        assert_eq!(defaults.model_label, "OPUS");
        assert_eq!(defaults.runtime_mode, "full-access");
    }

    #[test]
    fn a_project_default_beats_the_environment_default() {
        let providers = [
            provider("codex", "codex", "ready", &[("gpt", true)]),
            provider("claudeAgent", "claudeAgent", "ready", &[("opus", true)]),
        ];
        let settings = settings(json!({
            "projectSettingsFolded": true,
            "defaultModelSelection": { "instanceId": "codex", "model": "gpt" },
            "projectSettingsOverrides": { "p": {
                "defaultModelSelection": {
                    "instanceId": "claudeAgent", "model": "sonnet",
                    "options": [{ "id": "effort", "value": "high" }],
                },
                "defaultRuntimeMode": "approval-required",
            } },
        }));
        let defaults = new_thread_defaults(&providers, &settings, &project()).unwrap();
        assert_eq!(
            defaults.model_selection,
            json!({
                "instanceId": "claudeAgent", "model": "sonnet",
                "options": [{ "id": "effort", "value": "high" }],
            })
        );
        assert_eq!(defaults.runtime_mode, "approval-required");
    }

    #[test]
    fn a_project_that_disables_the_default_instance_falls_through() {
        let providers = [
            provider("codex", "codex", "ready", &[("gpt", true)]),
            provider("claudeAgent", "claudeAgent", "ready", &[("opus", true)]),
        ];
        let settings = settings(json!({
            "defaultModelSelection": { "instanceId": "codex", "model": "gpt" },
            "projectSettingsOverrides": { "p": {
                "providerInstances": { "codex": { "enabled": false } },
            } },
        }));
        let defaults = new_thread_defaults(&providers, &settings, &project()).unwrap();
        assert_eq!(defaults.model_selection["instanceId"], "claudeAgent");
    }

    #[test]
    fn unconfigured_instances_follow_the_driver_defaults() {
        let settings = settings(json!({
            "providerInstances": { "work": { "driver": "cursor" }, "mine": { "driver": "codex" } },
        }));
        let enabled = |instance: &str, driver: &str| {
            settings.instance_enabled(None, &provider(instance, driver, "ready", &[]))
        };
        assert!(enabled("codex", "codex"));
        assert!(!enabled("cursor", "cursor"));
        assert!(!enabled("work", "cursor"));
        assert!(enabled("mine", "codex"));
    }

    #[test]
    fn an_unsupported_runtime_mode_falls_back_to_a_supported_one() {
        let mut only_approval = provider("codex", "codex", "ready", &[("gpt", true)]);
        only_approval.supported_runtime_modes =
            Some(vec!["auto".into(), "approval-required".into()]);
        let defaults =
            new_thread_defaults(&[only_approval], &settings(json!({})), &project()).unwrap();
        assert_eq!(defaults.runtime_mode, "approval-required");
    }

    #[test]
    fn no_runnable_provider_means_no_defaults() {
        let mut off = provider("codex", "codex", "ready", &[("gpt", true)]);
        off.availability = Some("unavailable".into());
        assert!(new_thread_defaults(&[off], &settings(json!({})), &project()).is_none());
    }

    #[test]
    fn a_started_thread_picks_only_its_own_driver() {
        let mut second = provider("work", "codex", "error", &[("gpt", true)]);
        second.display_name = Some("Work".into());
        let providers = [
            second,
            provider("claudeAgent", "claudeAgent", "ready", &[("opus", true)]),
            provider("codex", "codex", "ready", &[("gpt", true)]),
        ];
        let settings = settings(json!({ "providerInstances": { "work": { "driver": "codex" } } }));
        let all = picker_providers(&providers, &settings, None, None);
        let ids: Vec<&str> = all.iter().map(|p| p.instance_id.as_str()).collect();
        assert_eq!(ids, ["codex", "work", "claudeAgent"]);
        assert!(!all[1].ready);
        let locked = picker_providers(&providers, &settings, None, Some("codex"));
        assert_eq!(locked.len(), 2);
    }

    #[test]
    fn picker_uses_current_custom_models_visibility_order_and_project_enablement() {
        let mut codex = provider(
            "codex",
            "codex",
            "ready",
            &[("a", true), ("b", false), ("removed", false)],
        );
        codex.models[2].is_custom = true;
        let providers = [
            codex,
            provider("claudeAgent", "claudeAgent", "ready", &[("opus", true)]),
        ];
        let settings = settings(json!({
            "providerInstances": { "codex": { "driver": "codex", "config": { "customModels": [" fresh ", { "slug": "named", "name": "Friendly" }, "fresh", "a", ""] } } },
            "providerModelPreferences": { "codex": { "hiddenModels": ["b"], "modelOrder": ["named", "fresh"] } },
            "projectSettingsOverrides": { "p": { "providerInstances": { "claudeAgent": { "enabled": false } } } },
        }));
        let entries = picker_providers(&providers, &settings, Some("p"), None);
        assert_eq!(entries.len(), 1);
        let models = &entries[0].models;
        assert_eq!(
            models
                .iter()
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>(),
            ["named", "fresh", "a"]
        );
        assert_eq!(models[0].name, "Friendly");
        assert!(models[0].is_custom);
    }

    #[test]
    fn null_model_preferences_from_the_server_mean_no_filtering() {
        let settings = settings(json!({ "providerModelPreferences": null }));
        let provider = provider("codex", "codex", "ready", &[("gpt", true)]);
        assert_eq!(picker_models(&provider, &settings), provider.models);
    }

    #[test]
    fn started_threads_block_provider_switches_and_locked_models() {
        let mut locked = provider("cursor", "cursor", "ready", &[]);
        locked.requires_new_thread_for_model_change = true;
        let providers = [provider("codex", "codex", "ready", &[]), locked];
        let codex = |model: &str| json!({ "instanceId": "codex", "model": model });
        let cursor = |model: &str| json!({ "instanceId": "cursor", "model": model });
        assert_eq!(
            model_change_block(&providers, false, &codex("a"), &cursor("b")),
            None
        );
        assert_eq!(
            model_change_block(&providers, true, &codex("a"), &codex("b")),
            None
        );
        assert_eq!(
            model_change_block(&providers, true, &codex("a"), &cursor("b")),
            Some("Start a new chat to switch providers")
        );
        assert_eq!(
            model_change_block(&providers, true, &cursor("a"), &cursor("b")),
            Some("Start a new chat to change models")
        );
    }

    #[test]
    fn runtime_modes_follow_the_provider() {
        assert_eq!(runtime_modes_for(None).len(), 4);
        let mut limited = provider("codex", "codex", "ready", &[]);
        limited.supported_runtime_modes =
            Some(vec!["full-access".into(), "approval-required".into()]);
        assert_eq!(
            runtime_modes_for(Some(&limited)),
            ["approval-required", "full-access"]
        );
    }

    #[test]
    fn switching_providers_resolves_an_unsupported_pending_mode() {
        let mut provider = provider("limited", "cursor", "ready", &[]);
        provider.supported_runtime_modes = Some(vec!["approval-required".into()]);
        assert_eq!(
            resolve_runtime_mode(Some(&provider), "full-access"),
            "approval-required"
        );
        assert_eq!(resolve_runtime_mode(None, "auto"), "auto");
    }

    #[test]
    fn reselecting_a_model_preserves_options_but_switching_clears_them() {
        let current = json!({ "instanceId": "codex", "model": "gpt", "options": [{ "id": "effort", "value": "high" }] });
        assert_eq!(select_model(&current, "codex", "gpt"), current);
        assert_eq!(
            select_model(&current, "codex", "other"),
            json!({ "instanceId": "codex", "model": "other" })
        );
        assert_eq!(
            select_model(&current, "work", "gpt"),
            json!({ "instanceId": "work", "model": "gpt" })
        );
    }

    #[test]
    fn display_names_follow_the_web() {
        let mut custom = provider("work_claude", "claudeAgent", "ready", &[]);
        assert_eq!(custom.display_name(), "Work Claude");
        custom.display_name = Some("Team".into());
        assert_eq!(custom.display_name(), "Team");
        assert_eq!(humanize_slug("claudeAgent"), "Claude Agent");
    }
}
