use anyhow::{Context, Result, anyhow};
use chrono_tz::Tz;
use std::env;

pub struct Config {
    pub slack_bot_token: String,
    pub slack_user_token: String,
    pub slack_signing_secret: String,
    pub owner_slack_user_id: String,
    pub slack_channel_ids: Vec<String>,
    pub scrapbook_channel_id: String,
    pub github_username: String,
    pub hackatime_username: String,
    pub scrapbook_username: String,
    pub timezone: Tz,
    pub ai_base_url: String,
    pub ai_model: String,
    pub ai_api_keys: Vec<String>,
    pub database_path: String,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let required = |key: &str| -> Result<String> {
            env::var(key).with_context(|| format!("missing required environment variable {key}"))
        };
        let split_list = |key: &str| -> Result<Vec<String>> {
            let values = required(key)?
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if values.is_empty() {
                return Err(anyhow!("{key} must contain at least one value"));
            }
            Ok(values)
        };
        let timezone_name = env::var("TIMEZONE").unwrap_or_else(|_| "Africa/Lagos".to_owned());
        let timezone = timezone_name
            .parse::<Tz>()
            .map_err(|_| anyhow!("TIMEZONE must be a valid IANA timezone"))?;

        Ok(Self {
            slack_bot_token: required("SLACK_BOT_TOKEN")?,
            slack_user_token: required("SLACK_USER_TOKEN")?,
            slack_signing_secret: required("SLACK_SIGNING_SECRET")?,
            owner_slack_user_id: required("OWNER_SLACK_USER_ID")?,
            slack_channel_ids: split_list("SLACK_CHANNEL_IDS")?,
            scrapbook_channel_id: required("SCRAPBOOK_CHANNEL_ID")?,
            github_username: required("GITHUB_USERNAME")?,
            hackatime_username: required("HACKATIME_USERNAME")?,
            scrapbook_username: required("SCRAPBOOK_USERNAME")?,
            timezone,
            ai_base_url: env::var("AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
            ai_model: required("AI_MODEL")?,
            ai_api_keys: split_list("AI_API_KEYS")?,
            database_path: env::var("DATABASE_PATH")
                .unwrap_or_else(|_| "data/drafts.sqlite3".to_owned()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".to_owned())
                .parse()
                .context("PORT must be a number")?,
        })
    }
}
