use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;
use std::collections::HashSet;

use crate::{
    app::AppState,
    models::{ActivityBundle, Evidence},
};

pub async fn gather(state: &AppState, date: NaiveDate) -> ActivityBundle {
    let (slack, github, hackatime, styles) = tokio::join!(
        slack_evidence(state, date),
        github_evidence(state, date),
        hackatime_evidence(state, date),
        scrapbook_style_samples(state),
    );

    let mut evidence = Vec::new();
    let mut unavailable_sources = Vec::new();
    for (name, result) in [
        ("Slack", slack),
        ("GitHub", github),
        ("Hackatime", hackatime),
    ] {
        match result {
            Ok(items) => evidence.extend(items),
            Err(error) => {
                tracing::warn!(source = name, error = %error, "activity source unavailable");
                unavailable_sources.push(name.to_owned());
            }
        }
    }
    let style_samples = match styles {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(error = %error, "could not fetch Scrapbook style samples");
            Vec::new()
        }
    };
    ActivityBundle {
        evidence,
        style_samples,
        unavailable_sources,
    }
}

fn day_bounds(date: NaiveDate, timezone: Tz) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let local_start = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("invalid date"))?;
    let next_local_start = (date + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("invalid date"))?;
    let start = timezone
        .from_local_datetime(&local_start)
        .single()
        .ok_or_else(|| anyhow!("could not resolve start of day in TIMEZONE"))?
        .with_timezone(&Utc);
    let end = timezone
        .from_local_datetime(&next_local_start)
        .single()
        .ok_or_else(|| anyhow!("could not resolve end of day in TIMEZONE"))?
        .with_timezone(&Utc);
    Ok((start, end))
}

async fn slack_evidence(state: &AppState, date: NaiveDate) -> Result<Vec<Evidence>> {
    let (start, end) = day_bounds(date, state.config.timezone)?;
    let oldest = start.timestamp().to_string();
    let latest = end.timestamp().to_string();
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for channel in &state.config.slack_channel_ids {
        let response = state
            .client
            .get("https://slack.com/api/conversations.history")
            .bearer_auth(&state.config.slack_bot_token)
            .query(&[
                ("channel", channel.as_str()),
                ("oldest", oldest.as_str()),
                ("latest", latest.as_str()),
                ("inclusive", "true"),
                ("limit", "999"),
            ])
            .send()
            .await
            .context("Slack history request failed")?;
        let value: Value = response
            .error_for_status()?
            .json()
            .await
            .context("Slack history returned invalid JSON")?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(anyhow!(
                "Slack history API rejected a request: {}",
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            ));
        }
        let messages = value
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for message in &messages {
            add_slack_message(
                &mut results,
                &mut seen,
                message,
                channel,
                &state.config.owner_slack_user_id,
                start.timestamp(),
                end.timestamp(),
            );
        }
        // Thread replies are stored separately from channel history, so read replies only for
        // active parent messages in this selected channel and keep the same user/date filter.
        for parent in messages
            .iter()
            .filter(|message| {
                message
                    .get("reply_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    > 0
            })
            .take(40)
        {
            let Some(ts) = parent.get("ts").and_then(Value::as_str) else {
                continue;
            };
            let replies: Value = state
                .client
                .get("https://slack.com/api/conversations.replies")
                .bearer_auth(&state.config.slack_bot_token)
                .query(&[("channel", channel.as_str()), ("ts", ts), ("limit", "999")])
                .send()
                .await
                .context("Slack thread request failed")?
                .error_for_status()?
                .json()
                .await
                .context("Slack thread returned invalid JSON")?;
            if replies.get("ok").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            for message in replies
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                add_slack_message(
                    &mut results,
                    &mut seen,
                    message,
                    channel,
                    &state.config.owner_slack_user_id,
                    start.timestamp(),
                    end.timestamp(),
                );
            }
        }
    }
    Ok(results)
}

fn add_slack_message(
    results: &mut Vec<Evidence>,
    seen: &mut HashSet<String>,
    message: &Value,
    channel: &str,
    owner: &str,
    start: i64,
    end: i64,
) {
    if message.get("user").and_then(Value::as_str) != Some(owner) {
        return;
    }
    let Some(ts) = message.get("ts").and_then(Value::as_str) else {
        return;
    };
    let Ok(seconds) = ts.parse::<f64>() else {
        return;
    };
    if seconds < start as f64 || seconds >= end as f64 || !seen.insert(ts.to_owned()) {
        return;
    }
    let Some(text) = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return;
    };
    let permalink_ts = ts.replace('.', "");
    results.push(Evidence {
        source: "Slack".to_owned(),
        title: format!("Message in selected channel {channel}"),
        detail: text.chars().take(1200).collect(),
        url: Some(format!(
            "https://app.slack.com/archives/{channel}/p{permalink_ts}"
        )),
    });
}

async fn github_evidence(state: &AppState, date: NaiveDate) -> Result<Vec<Evidence>> {
    let encoded = urlencoding::encode(&state.config.github_username);
    let response = state
        .client
        .get(format!(
            "https://api.github.com/users/{encoded}/events/public"
        ))
        .query(&[("per_page", "100")])
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("GitHub events request failed")?;
    let events: Value = response
        .error_for_status()?
        .json()
        .await
        .context("GitHub events returned invalid JSON")?;
    let mut results = Vec::new();
    for event in events.as_array().into_iter().flatten() {
        let Some(created) = event.get("created_at").and_then(Value::as_str) else {
            continue;
        };
        let Ok(created) = DateTime::parse_from_rfc3339(created) else {
            continue;
        };
        if created.with_timezone(&state.config.timezone).date_naive() != date {
            continue;
        }
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let repo = event
            .pointer("/repo/name")
            .and_then(Value::as_str)
            .unwrap_or("unknown repository");
        match kind {
            "PushEvent" => {
                for commit in event
                    .pointer("/payload/commits")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(12)
                {
                    let Some(message) = commit
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    else {
                        continue;
                    };
                    let url = commit
                        .get("sha")
                        .and_then(Value::as_str)
                        .map(|sha| format!("https://github.com/{repo}/commit/{sha}"));
                    results.push(Evidence {
                        source: "GitHub".to_owned(),
                        title: format!("Commit in {repo}"),
                        detail: message.chars().take(800).collect(),
                        url,
                    });
                }
            }
            "PullRequestEvent" => {
                if let Some(pr) = event.pointer("/payload/pull_request") {
                    let title = pr
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Pull request activity");
                    let action = event
                        .pointer("/payload/action")
                        .and_then(Value::as_str)
                        .unwrap_or("updated");
                    let url = pr
                        .get("html_url")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    results.push(Evidence {
                        source: "GitHub".to_owned(),
                        title: format!("Pull request {action}: {title}"),
                        detail: format!("Repository: {repo}"),
                        url,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(results)
}

async fn hackatime_evidence(state: &AppState, date: NaiveDate) -> Result<Vec<Evidence>> {
    let today = Utc::now()
        .with_timezone(&state.config.timezone)
        .date_naive();
    if date != today {
        return Ok(Vec::new());
    }
    let username = urlencoding::encode(&state.config.hackatime_username);
    let value: Value = state
        .client
        .get(format!(
            "https://hackatime.hackclub.com/api/v1/users/{username}/stats"
        ))
        .query(&[("range", "today")])
        .send()
        .await
        .context("Hackatime public stats request failed")?
        .error_for_status()?
        .json()
        .await
        .context("Hackatime stats returned invalid JSON")?;
    let data = value.get("data").unwrap_or(&value);
    let total = data
        .pointer("/grand_total/text")
        .and_then(Value::as_str)
        .or_else(|| data.pointer("/grand_total/digital").and_then(Value::as_str))
        .or_else(|| data.pointer("/grand_total/decimal").and_then(Value::as_str));
    let Some(total) = total.filter(|value| !value.is_empty() && !value.starts_with("00:00")) else {
        return Ok(Vec::new());
    };
    let projects = data
        .get("projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|project| {
            let name = project.get("name")?.as_str()?;
            let time = project
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| project.get("digital").and_then(Value::as_str))
                .unwrap_or("");
            Some(if time.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({time})")
            })
        })
        .take(10)
        .collect::<Vec<_>>();
    let detail = if projects.is_empty() {
        format!("Public profile reports {total} of coding time today.")
    } else {
        format!(
            "Public profile reports {total} today across: {}.",
            projects.join(", ")
        )
    };
    Ok(vec![Evidence {
        source: "Hackatime".to_owned(),
        title: format!("Hackatime activity for {}", state.config.hackatime_username),
        detail,
        url: Some(format!("https://hackatime.hackclub.com/{username}")),
    }])
}

async fn scrapbook_style_samples(state: &AppState) -> Result<Vec<String>> {
    let username = urlencoding::encode(&state.config.scrapbook_username);
    let profile: Value = state
        .client
        .get(format!(
            "https://scrapbook.hackclub.com/api/users/{username}"
        ))
        .query(&[("max", "12")])
        .send()
        .await
        .context("Scrapbook profile request failed")?
        .error_for_status()?
        .json()
        .await
        .context("Scrapbook profile returned invalid JSON")?;
    let posts = profile
        .get("posts")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Scrapbook profile did not include posts"))?;
    let samples = posts
        .iter()
        .filter_map(|post| {
            post.get("text")
                .and_then(Value::as_str)
                .or_else(|| post.get("messageText").and_then(Value::as_str))
                .or_else(|| post.get("description").and_then(Value::as_str))
                .map(|text| text.chars().take(700).collect::<String>())
                .filter(|text| !text.trim().is_empty())
        })
        .take(10)
        .collect();
    Ok(samples)
}
