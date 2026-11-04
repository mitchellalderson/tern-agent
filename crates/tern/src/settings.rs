//! The config editor's side of the runtime (`/v1/config/settings`): read
//! `config.toml` as settings, and change one at a time.

use serde_json::Value;
use tern_core::config::edit::ConfigFile;
use tern_core::config::settings::{self, ConfigView, Setting, SettingError};
use tern_core::models::Change;

use crate::runtime::Runtime;

impl Runtime {
    /// Every setting, as the file has it, compared with what this process
    /// started with.
    pub fn config_view(&self) -> ConfigView {
        let path = self.home.config_path();
        match ConfigFile::open(&path) {
            Ok(f) => settings::view(&f, &self.config),
            Err(e) => ConfigView {
                path: path.display().to_string(),
                error: Some(e.to_string()),
                sections: Vec::new(),
            },
        }
    }

    /// Set `key` to `value`, or back to its default with `None`. The file
    /// keeps its comments, and a value the config wouldn't load with is
    /// never written. Settings that apply live do from the next run.
    pub fn change_setting(
        &self,
        key: &str,
        value: Option<&Value>,
    ) -> Result<Setting, SettingError> {
        let mut file = ConfigFile::open(&self.home.config_path())?;
        match value {
            Some(v) => file.set_setting(key, v)?,
            None => {
                file.reset_setting(key)?;
            }
        }
        let config = file.save()?;
        self.set_live(config);
        if key == "default_model" {
            let _ = self.changes.send(Change::ProvidersChanged {});
        }
        tracing::info!(target: "audit", setting = %key, reset = value.is_none(), "config changed");
        let view = settings::view(&file, &self.config);
        settings::find(&view, key)
            .cloned()
            .ok_or_else(|| SettingError::Unknown(key.into()))
    }
}
