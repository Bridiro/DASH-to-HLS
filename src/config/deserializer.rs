use super::channels::ChannelConfig;
use super::users::UserConfig;
use log::error;
use std::fs;

pub struct Deserializer {
    channels_path: String,
    users_path: String,
}

impl Deserializer {
    pub fn new(channels_path: String, users_path: String) -> Self {
        Self {
            channels_path,
            users_path,
        }
    }

    pub fn load_channels(&self) -> anyhow::Result<ChannelConfig> {
        let data = load_file(&self.channels_path)?;

        match toml::from_str(&data) {
            Ok(config) => Ok(config),
            Err(e) => {
                error!("Failed to parse {}: {}", self.channels_path, e);
                Err(e.into())
            }
        }
    }

    pub fn load_users(&self) -> anyhow::Result<UserConfig> {
        let data = load_file(&self.users_path)?;

        match toml::from_str(&data) {
            Ok(config) => Ok(config),
            Err(e) => {
                error!("Failed to parse {}: {}", self.users_path, e);
                Err(e.into())
            }
        }
    }
}

fn load_file(path: &str) -> anyhow::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) => {
            error!("Failed to read {}: {}", path, e);
            Err(e.into())
        }
    }
}
