use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

use crate::models::{Draft, Evidence};

fn connect(path: &Path) -> Result<Connection> {
    Ok(Connection::open(path)?)
}

pub fn initialize(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let connection = connect(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS drafts (
           id TEXT PRIMARY KEY,
           owner_slack_user_id TEXT NOT NULL,
           date TEXT NOT NULL,
           text TEXT NOT NULL,
           evidence_json TEXT NOT NULL,
           unavailable_sources_json TEXT NOT NULL DEFAULT '[]',
           image_file_id TEXT,
           dm_channel_id TEXT,
           dm_message_ts TEXT,
           status TEXT NOT NULL,
           last_error TEXT
         );",
    )?;
    Ok(())
}

pub fn insert(path: &Path, draft: &Draft) -> Result<()> {
    let connection = connect(path)?;
    connection.execute(
        "INSERT INTO drafts (id, owner_slack_user_id, date, text, evidence_json, unavailable_sources_json, image_file_id, dm_channel_id, dm_message_ts, status, last_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            draft.id,
            draft.owner_slack_user_id,
            draft.date,
            draft.text,
            serde_json::to_string(&draft.evidence)?,
            serde_json::to_string(&draft.unavailable_sources)?,
            draft.image_file_id,
            draft.dm_channel_id,
            draft.dm_message_ts,
            draft.status,
            draft.last_error,
        ],
    )?;
    Ok(())
}

pub fn get(path: &Path, id: &str) -> Result<Option<Draft>> {
    let connection = connect(path)?;
    let row = connection
        .query_row(
            "SELECT id, owner_slack_user_id, date, text, evidence_json, unavailable_sources_json, image_file_id, dm_channel_id, dm_message_ts, status, last_error
             FROM drafts WHERE id = ?1",
            [id],
            |row| {
                let evidence_json: String = row.get(4)?;
                let evidence: Vec<Evidence> = serde_json::from_str(&evidence_json).unwrap_or_default();
                let unavailable_json: String = row.get(5)?;
                let unavailable_sources: Vec<String> = serde_json::from_str(&unavailable_json).unwrap_or_default();
                Ok(Draft {
                    id: row.get(0)?,
                    owner_slack_user_id: row.get(1)?,
                    date: row.get(2)?,
                    text: row.get(3)?,
                    evidence,
                    unavailable_sources,
                    image_file_id: row.get(6)?,
                    dm_channel_id: row.get(7)?,
                    dm_message_ts: row.get(8)?,
                    status: row.get(9)?,
                    last_error: row.get(10)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub fn save_dm_location(path: &Path, id: &str, channel_id: &str, message_ts: &str) -> Result<()> {
    let connection = connect(path)?;
    connection.execute(
        "UPDATE drafts SET dm_channel_id = ?2, dm_message_ts = ?3 WHERE id = ?1",
        params![id, channel_id, message_ts],
    )?;
    Ok(())
}

pub fn save_edit(path: &Path, id: &str, text: &str, image_file_id: &str) -> Result<()> {
    let connection = connect(path)?;
    connection.execute(
        "UPDATE drafts SET text = ?2, image_file_id = ?3, status = 'ready', last_error = NULL WHERE id = ?1",
        params![id, text, image_file_id],
    )?;
    Ok(())
}

pub fn claim_publish(path: &Path, id: &str) -> Result<bool> {
    let connection = connect(path)?;
    let changed = connection.execute(
        "UPDATE drafts SET status = 'publishing', last_error = NULL WHERE id = ?1 AND status = 'ready' AND image_file_id IS NOT NULL",
        [id],
    )?;
    Ok(changed == 1)
}

pub fn set_status(path: &Path, id: &str, status: &str, error: Option<&str>) -> Result<()> {
    let connection = connect(path)?;
    connection.execute(
        "UPDATE drafts SET status = ?2, last_error = ?3 WHERE id = ?1",
        params![id, status, error],
    )?;
    Ok(())
}
