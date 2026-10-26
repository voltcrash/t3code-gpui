//! Client preferences belong to the native app, separate from the Node
//! server's userdata/settings.json. Unknown keys survive older app versions.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLayout {
    #[default]
    Stacked,
    Split,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub appearance: Appearance,
    pub desktop_notifications_enabled: bool,
    pub plan_mode_enabled: bool,
    pub confirm_thread_archive: bool,
    pub confirm_thread_delete: bool,
    pub diff_layout: DiffLayout,
    pub word_wrap: bool,
    pub diff_ignore_whitespace: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            desktop_notifications_enabled: false,
            plan_mode_enabled: false,
            confirm_thread_archive: false,
            confirm_thread_delete: true,
            diff_layout: DiffLayout::Stacked,
            word_wrap: true,
            diff_ignore_whitespace: true,
            extra: BTreeMap::new(),
        }
    }
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn new(t3_home: &Path) -> Self {
        Self {
            path: t3_home.join("userdata/gpui-client-settings.json"),
        }
    }

    pub fn load(&self) -> io::Result<Preferences> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Preferences::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, preferences: &Preferences) -> io::Result<()> {
        let parent = self.path.parent().expect("preference file has a parent");
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".gpui-client-settings-{}.tmp", nonce()?));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            let bytes = serde_json::to_vec_pretty(preferences).map_err(io::Error::other)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn nonce() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("t3-gpui-prefs-{}", nonce().unwrap())))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_preferences_use_client_defaults_without_creating_files() {
        let home = Fixture::new();
        let store = Store::new(&home.0);
        assert_eq!(store.load().unwrap(), Preferences::default());
        assert!(!store.load().unwrap().plan_mode_enabled);
        assert!(!home.0.exists());
    }

    #[test]
    fn updates_survive_restarts_preserve_unknown_keys_and_leave_server_preferences_alone() {
        let home = Fixture::new();
        let store = Store::new(&home.0);
        fs::create_dir_all(home.0.join("userdata")).unwrap();
        let server = home.0.join("userdata/settings.json");
        fs::write(&server, b"server-owned").unwrap();
        fs::write(
            &store.path,
            br#"{"appearance":"dark","futureOption":{"enabled":true}}"#,
        )
        .unwrap();
        let mut preferences = store.load().unwrap();
        assert_eq!(preferences.appearance, Appearance::Dark);
        assert!(preferences.confirm_thread_delete);
        preferences.confirm_thread_archive = true;
        preferences.plan_mode_enabled = true;
        preferences.diff_layout = DiffLayout::Split;
        preferences.word_wrap = false;
        preferences.diff_ignore_whitespace = false;
        store.save(&preferences).unwrap();
        assert_eq!(Store::new(&home.0).load().unwrap(), preferences);
        assert_eq!(fs::read(&server).unwrap(), b"server-owned");
        let saved: Value = serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap();
        assert_eq!(saved["futureOption"]["enabled"], true);
        assert_eq!(fs::read_dir(home.0.join("userdata")).unwrap().count(), 2);
    }

    #[test]
    fn corrupt_or_incompatible_preferences_are_reported_without_replacing_them() {
        let home = Fixture::new();
        let store = Store::new(&home.0);
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        for bytes in [b"invalid json".as_slice(), br#"{"appearance":"unknown"}"#] {
            fs::write(&store.path, bytes).unwrap();
            assert!(store.load().is_err());
            assert_eq!(fs::read(&store.path).unwrap(), bytes);
        }
    }
}
