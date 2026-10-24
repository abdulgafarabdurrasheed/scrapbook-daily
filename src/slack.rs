use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::{app::AppState, models::Draft, store};

async fn api(state: &AppState, token: &str, method: &str, payload: Value) -> Result<Value> {
    let response = state
        .client
        .post(format!("https://slack.com/api/{method}"))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("Slack {method} request failed"))?;
    let status = response.status();
    let value: Value = response
        .json()
        .await
        .with_context(|| format!("Slack {method} returned invalid JSON"))?;
    if !status.is_success() || value.get("ok").and_then(Value::as_bool) != Some(true) {
        let reason = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown Slack API error");
        return Err(anyhow!("Slack {method} failed ({status}): {reason}"));
    }
    Ok(value)
}

pub async fn send_notice(state: &AppState, user_id: &str, message: &str) -> Result<()> {
    let opened = api(
        state,
        &state.config.slack_bot_token,
        "conversations.open",
        json!({"users": user_id}),
    )
    .await?;
    let channel = opened
        .pointer("/channel/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Slack did not return a DM channel"))?;
    api(
        state,
        &state.config.slack_bot_token,
        "chat.postMessage",
        json!({
            "channel": channel,
            "text": message,
            "mrkdwn": true,
        }),
    )
    .await?;
    Ok(())
}

pub async fn send_draft_dm(state: &AppState, draft: &mut Draft) -> Result<()> {
    let opened = api(
        state,
        &state.config.slack_bot_token,
        "conversations.open",
        json!({"users": draft.owner_slack_user_id}),
    )
    .await?;
    let channel = opened
        .pointer("/channel/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Slack did not return a DM channel"))?;
    let response = api(
        state,
        &state.config.slack_bot_token,
        "chat.postMessage",
        json!({
            "channel": channel,
            "text": format!("Your Scrapbook draft for {} is ready to review.", draft.date),
            "blocks": draft_blocks(draft, None),
        }),
    )
    .await?;
    let ts = response
        .pointer("/ts")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Slack did not return the DM message timestamp"))?;
    draft.dm_channel_id = Some(channel.to_owned());
    draft.dm_message_ts = Some(ts.to_owned());
    store::save_dm_location(&state.db_path, &draft.id, channel, ts)?;
    Ok(())
}

pub async fn update_draft_dm(state: &AppState, draft: &Draft, note: Option<&str>) -> Result<()> {
    let (Some(channel), Some(ts)) = (&draft.dm_channel_id, &draft.dm_message_ts) else {
        return Ok(());
    };
    api(
        state,
        &state.config.slack_bot_token,
        "chat.update",
        json!({
            "channel": channel,
            "ts": ts,
            "text": note.unwrap_or("Your Scrapbook draft is ready to review."),
            "blocks": draft_blocks(draft, note),
        }),
    )
    .await?;
    Ok(())
}

fn draft_blocks(draft: &Draft, note: Option<&str>) -> Vec<Value> {
    let mut blocks = vec![
        json!({"type":"header","text":{"type":"plain_text","text":format!("Scrapbook draft · {}", draft.date)}}),
        json!({"type":"section","text":{"type":"mrkdwn","text":draft.text}}),
    ];
    let evidence = draft
        .evidence
        .iter()
        .take(12)
        .map(|item| {
            let title = escape_mrkdwn(&item.title);
            let detail = escape_mrkdwn(&item.detail.chars().take(180).collect::<String>());
            match &item.url {
                Some(url) => format!("• <{}|{}> — {}", url, title, detail),
                None => format!("• {} — {}", title, detail),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !evidence.is_empty() {
        blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":format!("*Evidence used*\n{}", evidence.chars().take(2500).collect::<String>())}]}));
    }
    if !draft.unavailable_sources.is_empty() {
        blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":format!("Could not access: {}", draft.unavailable_sources.join(", "))}]}));
    }
    if let Some(note) = note {
        blocks.push(
            json!({"type":"context","elements":[{"type":"mrkdwn","text":escape_mrkdwn(note)}]}),
        );
    }
    if draft.status == "published" {
        blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":"Published to `#scrapbook`."}]}));
    } else if draft.status == "publishing" {
        blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":"Publishing to `#scrapbook`…"}]}));
    } else {
        let mut buttons = Vec::new();
        if draft.image_file_id.is_some() && draft.status == "ready" {
            buttons.push(json!({"type":"button","action_id":"publish_draft","text":{"type":"plain_text","text":"Publish to Scrapbook"},"style":"primary","value":draft.id}));
        }
        buttons.push(json!({"type":"button","action_id":"edit_draft","text":{"type":"plain_text","text":"Edit and attach an image"},"value":draft.id}));
        blocks.push(json!({"type":"actions","block_id":"draft_actions","elements":buttons}));
        if draft.image_file_id.is_none() {
            blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":"Scrapbook needs an image. Add one in the edit form to enable publishing."}]}));
        }
    }
    blocks
}

fn escape_mrkdwn(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub async fn open_edit_modal(state: &AppState, trigger_id: &str, draft: &Draft) -> Result<()> {
    let initial_text = draft.text.chars().take(3000).collect::<String>();
    api(state, &state.config.slack_bot_token, "views.open", json!({
        "trigger_id": trigger_id,
        "view": {
            "type": "modal",
            "callback_id": "scrapbook_daily_edit",
            "private_metadata": draft.id,
            "title": {"type":"plain_text","text":"Edit Scrapbook draft"},
            "submit": {"type":"plain_text","text":"Save draft"},
            "close": {"type":"plain_text","text":"Cancel"},
            "blocks": [
                {
                    "type":"input",
                    "block_id":"draft_text_block",
                    "label":{"type":"plain_text","text":"Post text"},
                    "element":{"type":"plain_text_input","action_id":"draft_text_input","multiline":true,"max_length":3000,"initial_value":initial_text}
                },
                {
                    "type":"input",
                    "block_id":"image_block",
                    "optional":true,
                    "label":{"type":"plain_text","text":"Scrapbook image"},
                    "element":{"type":"file_input","action_id":"image_input","filetypes":["png","jpg","jpeg","gif","webp"],"max_files":1}
                }
            ]
        }
    })).await?;
    Ok(())
}

pub async fn publish_to_scrapbook(state: &AppState, draft: &Draft) -> Result<()> {
    let file_id = draft
        .image_file_id
        .as_deref()
        .ok_or_else(|| anyhow!("Choose an image before publishing"))?;
    let file_info = api(
        state,
        &state.config.slack_bot_token,
        "files.info",
        json!({"file": file_id}),
    )
    .await?;
    let file = file_info
        .get("file")
        .ok_or_else(|| anyhow!("Slack did not return the selected image"))?;
    let mimetype = file
        .get("mimetype")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !mimetype.starts_with("image/") {
        return Err(anyhow!("The selected file is not an image"));
    }
    let size = file.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size == 0 || size > 25_000_000 {
        return Err(anyhow!("Choose an image smaller than 25 MB"));
    }
    let filename = file
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("scrapbook-image.png");
    let download_url = file
        .get("url_private_download")
        .and_then(Value::as_str)
        .or_else(|| file.get("url_private").and_then(Value::as_str))
        .ok_or_else(|| anyhow!("Slack did not return a private download URL for the image"))?;
    let image = state
        .client
        .get(download_url)
        .bearer_auth(&state.config.slack_bot_token)
        .send()
        .await
        .context("Could not download the selected Slack image")?
        .error_for_status()?
        .bytes()
        .await
        .context("Could not read the selected Slack image")?;

    let upload_info = api(
        state,
        &state.config.slack_user_token,
        "files.getUploadURLExternal",
        json!({
            "filename": filename,
            "length": image.len(),
        }),
    )
    .await?;
    let upload_url = upload_info
        .get("upload_url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Slack did not return an image upload URL"))?;
    let new_file_id = upload_info
        .get("file_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Slack did not return an upload file ID"))?;
    state
        .client
        .post(upload_url)
        .header("Content-Type", mimetype)
        .body(image)
        .send()
        .await
        .context("Image upload to Slack failed")?
        .error_for_status()
        .context("Image upload to Slack was rejected")?;

    api(
        state,
        &state.config.slack_user_token,
        "files.completeUploadExternal",
        json!({
            "files": [{"id": new_file_id, "title": filename}],
            "channel_id": state.config.scrapbook_channel_id,
            "initial_comment": draft.text,
        }),
    )
    .await?;
    Ok(())
}
