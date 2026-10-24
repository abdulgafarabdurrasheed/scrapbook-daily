# scrapbook-daily

A personal Rust Slack bot that reconstructs a day's work from your own Slack messages, public GitHub activity, and public Hackatime stats. It uses recent public Scrapbook posts as style examples, creates a sourced draft, and sends it to you in a Slack DM.

Nothing is published automatically. The DM has an **Edit and attach an image** button. Scrapbook's Slack integration requires media; after you add an image and save, a **Publish to Scrapbook** button appears. Clicking it uploads the image and posts your edited text to `#scrapbook` as your Slack user.

This is a single-user MVP. It only processes messages written by the configured owner in the selected channels. The bot must be a member of those channels. It stores draft text, evidence links, and the selected image's Slack file ID in a local SQLite database.

## Setup

1. Install Rust (stable) and create a Slack app for your workspace.
2. Import `slack-app-manifest.json` into a Slack app, replace both `https://example.com` request URLs with your HTTPS host, and install the app. The manifest defines `/scrapbook-draft` and the minimum bot/user scopes.
3. In OAuth & Permissions, configure bot scopes `commands`, `chat:write`, `im:write`, `channels:history`, and `files:read`. Enable Interactivity and set its request URL to `https://YOUR-HOST/slack/interactions`. `views.open` does not need its own scope.
4. Configure a user OAuth token for the account that owns the Scrapbook posts, with user scopes `chat:write` and `files:write`. The user must already be a member of `#scrapbook`. Publishing is performed with this user token so Scrapbook sees the actual author.
5. Invite the bot to each public channel you choose to include and set their IDs in `SLACK_CHANNEL_IDS`. Avoid channels whose messages you do not want sent to your AI provider.
6. Copy `.env.example` to `.env`, fill in your values, and keep `.env` private. On startup, the app checks that the user token belongs to `OWNER_SLACK_USER_ID` and the same workspace as the bot token. `AI_API_KEYS` accepts comma-separated keys and rotates one key per draft request. It does not retry a failed or rate-limited request with another key.
7. Run `cargo run --release`. Expose the server over HTTPS for Slack using your preferred tunnel or host.
8. In Slack, run `/scrapbook-draft` or `/scrapbook-draft 2026-10-24`.

## Data and privacy

- Slack history is limited to the channel IDs in your configuration, the requested date, and messages written by the configured owner. The bot must be a member of each channel.
- Only public GitHub events and public Hackatime profile stats are fetched. If Hackatime privacy settings hide the profile, that source is omitted.
- Scrapbook style samples are fetched from the configured public profile.
- The AI provider receives the collected evidence and public style samples to produce the draft. Use an endpoint and keys you trust.
- The bot never publishes unless the owner clicks **Publish to Scrapbook**. It refuses to publish without a user-selected image.
- Slack request signatures are checked before slash commands or interactions are accepted.

## Current scope

The GitHub connector uses GitHub's public events feed, so it captures recent push and pull request activity rather than searching every repository's full commit history. Hackatime's public stats are used for the current day; historical exact-day stats are omitted. The first version has one configured owner and one Slack workspace.
