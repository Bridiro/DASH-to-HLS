use serde::Deserialize;

#[derive(Deserialize)]
pub struct ChannelTOML {
    pub id: String,
    pub name: String,
    pub url: String,
    pub key: String,
}

#[derive(Deserialize)]
pub struct ChannelConfig {
    pub channel: Vec<ChannelTOML>,
}

