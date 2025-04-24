use serde::Deserialize;

#[derive(Deserialize)]
pub struct UserTOML {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct UserConfig {
    pub user: Vec<UserTOML>,
}
