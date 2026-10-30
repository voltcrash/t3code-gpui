//! Unsent text is native client state; it never enters the server's settings file.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedDrafts {
    pub threads: BTreeMap<String, String>,
    pub new_threads: BTreeMap<String, t3_client::new_thread_drafts::SavedDraft>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

pub struct Store {
    path: PathBuf,
}
impl Store {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join("userdata/gpui-message-drafts.json"),
        }
    }
    pub fn load(&self) -> io::Result<SavedDrafts> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SavedDrafts::default()),
            Err(error) => Err(error),
        }
    }
    pub fn save(&self, saved: &SavedDrafts) -> io::Result<()> {
        crate::preferences::save_json(&self.path, saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "t3-message-drafts-{}",
                crate::preferences::nonce().unwrap()
            )))
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_drafts_do_not_create_files_or_touch_server_state() {
        let home = Home::new();
        assert_eq!(Store::new(&home.0).load().unwrap(), SavedDrafts::default());
        assert!(!home.0.exists());
    }

    #[test]
    fn text_and_future_fields_survive_restarts_while_server_settings_stay_untouched() {
        let home = Home::new();
        let store = Store::new(&home.0);
        fs::create_dir_all(home.0.join("userdata")).unwrap();
        let server = home.0.join("userdata/settings.json");
        fs::write(&server, "server-owned").unwrap();
        fs::write(
            &store.path,
            br#"{"threads":{"first":"Old text"},"future":{"enabled":true}}"#,
        )
        .unwrap();
        let mut saved = store.load().unwrap();
        saved
            .threads
            .insert("first".into(), "Exact text 🌱\nSecond line".into());
        saved
            .threads
            .insert("second".into(), "Another draft".into());
        saved.new_threads.insert(
            "draft".into(),
            t3_client::new_thread_drafts::SavedDraft {
                project_id: "first".into(),
                text: "Different project draft".into(),
                order: 1,
            },
        );
        store.save(&saved).unwrap();
        assert_eq!(Store::new(&home.0).load().unwrap(), saved);
        assert_eq!(fs::read_to_string(&server).unwrap(), "server-owned");
        let json: Value = serde_json::from_slice(&fs::read(&store.path).unwrap()).unwrap();
        assert_eq!(json["future"]["enabled"], true);
        saved.threads.clear();
        store.save(&saved).unwrap();
        assert!(store.load().unwrap().threads.is_empty());
        assert_eq!(
            store.load().unwrap().new_threads["draft"].text,
            "Different project draft"
        );
        assert_eq!(fs::read_dir(home.0.join("userdata")).unwrap().count(), 2);
    }

    #[test]
    fn corrupt_drafts_and_write_errors_leave_existing_data_untouched() {
        let home = Home::new();
        let store = Store::new(&home.0);
        fs::create_dir_all(home.0.join("userdata")).unwrap();
        fs::write(&store.path, "corrupt").unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read_to_string(&store.path).unwrap(), "corrupt");
        fs::remove_file(&store.path).unwrap();
        fs::create_dir(&store.path).unwrap();
        fs::write(store.path.join("keep"), "keep me").unwrap();
        assert!(store.save(&SavedDrafts::default()).is_err());
        assert_eq!(
            fs::read_to_string(store.path.join("keep")).unwrap(),
            "keep me"
        );
        assert_eq!(fs::read_dir(home.0.join("userdata")).unwrap().count(), 1);
    }
}
