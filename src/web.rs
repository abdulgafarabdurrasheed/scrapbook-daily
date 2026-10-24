use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chrono::{NaiveDate, Utc};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use url::form_urlencoded;
use uuid::Uuid;

use crate::{ai, app::AppState, models::Draft, slack, sources, store};

pub async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}

pub async fn slash_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    if !verify_signature(&headers, &body, &state.config.slack_signing_secret) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let fields: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
    let user_id = fields
        .get("user_id")
        .cloned()
        .ok_or(StatusCode::BAD_REQUEST)?;
    if user_id != state.config.owner_slack_user_id {
        return Ok(Json(
            json!({"response_type":"ephemeral","text":"This personal bot is configured for its owner."}),
        ));
    }
    let date = if let Some(text) = fields.get("text").filter(|text| !text.trim().is_empty()) {
        match NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => {
                return Ok(Json(
                    json!({"response_type":"ephemeral","text":"Usage: `/scrapbook-draft [YYYY-MM-DD]`"}),
                ));
            }
        }
    } else {
        Utc::now()
            .with_timezone(&state.config.timezone)
            .date_naive()
    };
    let date_text = date.to_string();
    let background_state = state.clone();
    tokio::spawn(async move {
        create_and_send_draft(background_state, user_id, date, date_text).await;
    });
    Ok(Json(
        json!({"response_type":"ephemeral","text":"Got it. I’m gathering your activity and will DM you a draft to review."}),
    ))
}

pub async fn interaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    if !verify_signature(&headers, &body, &state.config.slack_signing_secret) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let fields: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
    let raw = fields.get("payload").ok_or(StatusCode::BAD_REQUEST)?;
    let payload: Value = serde_json::from_str(raw).map_err(|_| StatusCode::BAD_REQUEST)?;
    let kind = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match kind {
        "block_actions" => handle_block_action(&state, &payload)
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "Slack interaction failed");
                StatusCode::BAD_REQUEST
            })?,
        "view_submission" => return handle_view_submission(&state, &payload).await,
        _ => {}
    }
    Ok(Json(json!({})))
}

async fn create_and_send_draft(
    state: AppState,
    user_id: String,
    date: NaiveDate,
    date_text: String,
) {
    let bundle = sources::gather(&state, date).await;
    if bundle.evidence.is_empty() {
        let mut message = format!(
            "I couldn't find any activity evidence for {date_text}, so I didn't make a post."
        );
        if !bundle.unavailable_sources.is_empty() {
            message.push_str(&format!(
                " I couldn't reach: {}.",
                bundle.unavailable_sources.join(", ")
            ));
        }
        if let Err(error) = slack::send_notice(&state, &user_id, &message).await {
            tracing::error!(error = %error, "could not DM empty-activity notice");
        }
        return;
    }
    let text = match ai::write_draft(&state, &date_text, &bundle.evidence, &bundle.style_samples)
        .await
    {
        Ok(text) => text,
        Err(error) => {
            tracing::warn!(error = %error, "AI did not create a draft");
            let message =
                format!("I found activity for {date_text}, but couldn't create a draft: {error}");
            if let Err(send_error) = slack::send_notice(&state, &user_id, &message).await {
                tracing::error!(error = %send_error, "could not DM draft error");
            }
            return;
        }
    };
    let mut draft = Draft {
        id: Uuid::new_v4().to_string(),
        owner_slack_user_id: user_id.clone(),
        date: date_text,
        text,
        evidence: bundle.evidence,
        unavailable_sources: bundle.unavailable_sources,
        image_file_id: None,
        dm_channel_id: None,
        dm_message_ts: None,
        status: "draft".to_owned(),
        last_error: None,
    };
    if let Err(error) = store::insert(&state.db_path, &draft) {
        tracing::error!(error = %error, "could not save generated draft");
        let _ = slack::send_notice(&state, &user_id, "I couldn't save your draft locally. Check the bot's database permissions and try again.").await;
        return;
    }
    if let Err(error) = slack::send_draft_dm(&state, &mut draft).await {
        tracing::error!(error = %error, "could not DM generated draft");
        let _ = slack::send_notice(&state, &user_id, "Your draft was saved, but I couldn't send it in Slack. Check the bot's DM permissions.").await;
    }
}

async fn handle_block_action(state: &AppState, payload: &Value) -> anyhow::Result<()> {
    let user_id = payload
        .pointer("/user/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("interaction has no user ID"))?;
    let action = payload
        .pointer("/actions/0/action_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let draft_id = payload
        .pointer("/actions/0/value")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("action has no draft ID"))?;
    let draft =
        store::get(&state.db_path, draft_id)?.ok_or_else(|| anyhow::anyhow!("draft not found"))?;
    if user_id != draft.owner_slack_user_id {
        return Err(anyhow::anyhow!(
            "only the draft owner can edit or publish it"
        ));
    }
    match action {
        "edit_draft" => {
            if draft.status == "published" || draft.status == "publishing" {
                return Ok(());
            }
            let trigger_id = payload
                .get("trigger_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("action has no trigger ID"))?;
            slack::open_edit_modal(state, trigger_id, &draft).await?;
        }
        "publish_draft" => {
            let publish_state = state.clone();
            let id = draft.id.clone();
            tokio::spawn(async move {
                publish_draft(publish_state, id).await;
            });
        }
        _ => {}
    }
    Ok(())
}

async fn handle_view_submission(
    state: &AppState,
    payload: &Value,
) -> Result<Json<Value>, StatusCode> {
    let user_id = payload
        .pointer("/user/id")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let view = payload.get("view").ok_or(StatusCode::BAD_REQUEST)?;
    let draft_id = view
        .get("private_metadata")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let Some(draft) =
        store::get(&state.db_path, draft_id).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    else {
        return Ok(Json(
            json!({"response_action":"errors","errors":{"draft_text_block":"This draft no longer exists."}}),
        ));
    };
    if user_id != draft.owner_slack_user_id {
        return Ok(Json(
            json!({"response_action":"errors","errors":{"draft_text_block":"Only the draft owner can edit this post."}}),
        ));
    }
    let values = view
        .pointer("/state/values")
        .ok_or(StatusCode::BAD_REQUEST)?;
    let text = values
        .pointer("/draft_text_block/draft_text_input/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if text.is_empty() {
        return Ok(Json(
            json!({"response_action":"errors","errors":{"draft_text_block":"Add some post text before saving."}}),
        ));
    }
    if text.chars().count() > 3_000 {
        return Ok(Json(
            json!({"response_action":"errors","errors":{"draft_text_block":"Keep the post under 3,000 characters."}}),
        ));
    }
    let selected_image = values
        .pointer("/image_block/image_input/files/0/id")
        .and_then(Value::as_str);
    let Some(image_id) = selected_image.or(draft.image_file_id.as_deref()) else {
        return Ok(Json(
            json!({"response_action":"errors","errors":{"image_block":"Attach an image before saving so the post can be published to Scrapbook."}}),
        ));
    };
    store::save_edit(&state.db_path, draft_id, text, image_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let update_state = state.clone();
    let update_id = draft_id.to_owned();
    tokio::spawn(async move {
        match store::get(&update_state.db_path, &update_id) {
            Ok(Some(updated)) => {
                if let Err(error) = slack::update_draft_dm(
                    &update_state,
                    &updated,
                    Some("Draft saved. Review it, then publish when you’re ready."),
                )
                .await
                {
                    tracing::warn!(error = %error, "could not refresh draft DM");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::error!(error = %error, "could not reload saved draft"),
        }
    });
    Ok(Json(json!({})))
}

async fn publish_draft(state: AppState, draft_id: String) {
    match store::claim_publish(&state.db_path, &draft_id) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::error!(error = %error, "could not claim draft for publishing");
            return;
        }
    }
    let Some(mut draft) = (match store::get(&state.db_path, &draft_id) {
        Ok(draft) => draft,
        Err(error) => {
            tracing::error!(error = %error, "could not load draft for publishing");
            return;
        }
    }) else {
        return;
    };
    draft.status = "publishing".to_owned();
    let _ = slack::update_draft_dm(
        &state,
        &draft,
        Some("Publishing your post to `#scrapbook`…"),
    )
    .await;

    match slack::publish_to_scrapbook(&state, &draft).await {
        Ok(()) => {
            if let Err(error) = store::set_status(&state.db_path, &draft_id, "published", None) {
                tracing::error!(error = %error, "post was published but local status could not be updated");
                return;
            }
            if let Ok(Some(published)) = store::get(&state.db_path, &draft_id) {
                if let Err(error) =
                    slack::update_draft_dm(&state, &published, Some("Published to `#scrapbook`."))
                        .await
                {
                    tracing::warn!(error = %error, "could not update published draft DM");
                }
            }
        }
        Err(error) => {
            let message = error.to_string();
            tracing::warn!(error = %message, "Scrapbook publish failed");
            let _ = store::set_status(&state.db_path, &draft_id, "ready", Some(&message));
            if let Ok(Some(ready)) = store::get(&state.db_path, &draft_id) {
                let note = format!(
                    "Publish failed: {message}. You can edit the draft or retry after fixing the Slack permissions."
                );
                if let Err(update_error) = slack::update_draft_dm(&state, &ready, Some(&note)).await
                {
                    tracing::warn!(error = %update_error, "could not show publish error in Slack");
                }
            }
        }
    }
}

fn verify_signature(headers: &HeaderMap, body: &[u8], secret: &str) -> bool {
    let Some(timestamp) = headers
        .get("x-slack-request-timestamp")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(signature) = headers
        .get("x-slack-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(timestamp_number) = timestamp.parse::<i64>() else {
        return false;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };
    if now.as_secs().abs_diff(timestamp_number as u64) > 300 {
        return false;
    }
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));
    expected.as_bytes().ct_eq(signature.as_bytes()).unwrap_u8() == 1
}
