use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub source: String,
    pub title: String,
    pub detail: String,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub owner_slack_user_id: String,
    pub date: String,
    pub text: String,
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub unavailable_sources: Vec<String>,
    pub image_file_id: Option<String>,
    pub dm_channel_id: Option<String>,
    pub dm_message_ts: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
}

pub struct ActivityBundle {
    pub evidence: Vec<Evidence>,
    pub style_samples: Vec<String>,
    pub unavailable_sources: Vec<String>,
}
