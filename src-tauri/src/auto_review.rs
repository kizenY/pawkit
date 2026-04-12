use crate::plog;
use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::AutoReviewConfig;
use crate::slack_bridge::{SessionThreadMap, SlackBridge};
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex, Notify};

/// Build a reqwest client with GitHub API auth headers.
/// Token is obtained from `gh auth token` at startup.
async fn get_github_token(account: &Option<String>) -> Result<String, String> {
    let mut args = vec!["auth", "token"];
    let user_flag;
    if let Some(ref acct) = account {
        user_flag = acct.clone();
        args.push("-u");
        args.push(&user_flag);
    }

    #[cfg(target_os = "windows")]
    let output = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut all = vec!["/C", "gh"];
        all.extend_from_slice(&args);
        tokio::process::Command::new("cmd")
            .args(all)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .await
    };
    #[cfg(not(target_os = "windows"))]
    let output = {
        tokio::process::Command::new("gh")
            .args(&args)
            .output()
            .await
    };

    let output = output.map_err(|e| format!("Failed to run gh auth token: {}", e))?;
    if !output.status.success() {
        return Err(format!("gh auth token failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn github_client(token: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(AUTHORIZATION, format!("Bearer {}", token).parse().expect("valid auth header"));
    headers.insert(ACCEPT, "application/vnd.github+json".parse().expect("valid accept header"));
    headers.insert(USER_AGENT, "Pawkit".parse().expect("valid user-agent header"));
    headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().expect("valid api-version header"));
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build reqwest client")
}

/// Get the authenticated GitHub user's login name
async fn get_github_username(client: &reqwest::Client) -> Result<String, String> {
    let resp = client
        .get("https://api.github.com/user")
        .send().await
        .map_err(|e| format!("GitHub user API failed: {}", e))?;
    let body: serde_json::Value = resp.json().await
        .map_err(|e| format!("Failed to parse user response: {}", e))?;
    body["login"].as_str()
        .map(String::from)
        .ok_or_else(|| "No login in user response".to_string())
}

/// A review item detected by the polling loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: String,
    pub repo: String,
    pub pr_number: u64,
    pub title: String,
    pub url: String,
    pub item_type: String, // "review_request", "mention", or "comment"
    pub body: String,      // comment body for context display
    #[serde(default)]
    pub is_own_pr: bool,
    #[serde(default)]
    pub notification_id: String,
    /// Slack thread_ts for posting review results (set when notified in away mode)
    #[serde(default)]
    pub slack_thread_ts: Option<String>,
}

/// Tracks which items we've already notified about
type SeenItems = Arc<Mutex<HashSet<String>>>;

/// Channel for approved items waiting to be processed
type ApprovedTx = mpsc::Sender<ReviewItem>;
type ApprovedRx = mpsc::Receiver<ReviewItem>;

/// Pending review items waiting for user decision
pub type PendingReviewItems = Arc<Mutex<Vec<ReviewItem>>>;

/// Manual trigger for immediate poll
pub type ManualPollTrigger = Arc<Notify>;

/// Start the auto-review background system
pub fn start_auto_review(
    app_handle: tauri::AppHandle,
    config: AutoReviewConfig,
    pending_items: PendingReviewItems,
    slack: Option<Arc<SlackBridge>>,
    is_away: Arc<AtomicBool>,
    manual_trigger: ManualPollTrigger,
    session_thread_map: SessionThreadMap,
) {
    if !config.enabled {
        plog!("[Pawkit] Auto-review disabled");
        return;
    }
    if config.repos.is_empty() {
        plog!("[Pawkit] Auto-review enabled for ALL repos (no repo filter)");
    } else {
        plog!("[Pawkit] Auto-review enabled for repos: {:?}", config.repos);
    }

    let (approved_tx, approved_rx) = mpsc::channel::<ReviewItem>(32);
    let seen: SeenItems = Arc::new(Mutex::new(HashSet::new()));

    // Get GitHub token once and share client across both tasks
    let poll_handle = app_handle.clone();
    let poll_config = config.clone();
    let poll_seen = seen.clone();
    let poll_pending = pending_items.clone();
    let poll_slack = slack.clone();
    let poll_away = is_away.clone();
    let proc_config = config.clone();
    let proc_handle = app_handle.clone();
    let proc_slack = slack.clone();
    let proc_away = is_away.clone();
    tauri::async_runtime::spawn(async move {
        // Get GitHub token once at startup
        let token = match get_github_token(&poll_config.gh_account).await {
            Ok(t) => {
                plog!("[Pawkit] GitHub token acquired");
                t
            }
            Err(e) => {
                plog!("[Pawkit] Failed to get GitHub token: {}. Auto-review polling disabled.", e);
                return;
            }
        };
        let client = github_client(&token);
        let client2 = client.clone();

        // Get authenticated user's login for determining PR ownership
        let username = match get_github_username(&client).await {
            Ok(u) => {
                plog!("[Pawkit] GitHub user: {}", u);
                u
            }
            Err(e) => {
                plog!("[Pawkit] Failed to get GitHub username: {}. Auto-review polling disabled.", e);
                return;
            }
        };

        // Task 1: Poll GitHub notifications
        tauri::async_runtime::spawn(async move {
            let interval = Duration::from_secs(poll_config.interval_minutes * 60);
            // Initial short delay before first poll
            tokio::time::sleep(Duration::from_secs(10)).await;

            loop {
                if let Err(e) = poll_github(
                    &client, &poll_handle, &poll_config, &poll_seen, &poll_pending,
                    &poll_slack, &poll_away, &username,
                ).await {
                    plog!("[Pawkit] Auto-review poll error: {}", e);
                }
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {},
                    _ = manual_trigger.notified() => {
                        plog!("[Pawkit] Manual check-pr triggered");
                    },
                }
            }
        });

        // Task 2: Process approved items with Claude Code
        process_approved_items(approved_rx, &proc_config, &proc_handle, &proc_slack, &proc_away, &client2, session_thread_map).await;
    });

    // Store the sender so approved items can be sent from the Tauri command
    let tx = Box::new(approved_tx);
    let tx_ptr = Box::into_raw(tx);
    unsafe {
        APPROVED_TX = tx_ptr as *const () as *mut ();
    }
}

// Global sender for approved items (set once on startup)
static mut APPROVED_TX: *mut () = std::ptr::null_mut();

pub fn get_approved_sender() -> Option<ApprovedTx> {
    unsafe {
        if APPROVED_TX.is_null() {
            None
        } else {
            let tx = &*(APPROVED_TX as *const ApprovedTx);
            Some(tx.clone())
        }
    }
}

/// Notify about a review item — via Slack in away mode, via cat UI in home mode.
/// Returns the Slack thread_ts if posted in away mode.
async fn notify_review_item(
    app_handle: &tauri::AppHandle,
    item: &ReviewItem,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
) -> Option<String> {
    let type_label = if item.is_own_pr {
        "My PR Update"
    } else {
        match item.item_type.as_str() {
            "review_request" => "Review Request",
            "mention" => "@Mention",
            "comment" => "New Comment",
            _ => "Notification",
        }
    };

    let mut thread_ts = None;
    if is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = slack {
            let msg = format!(
                "📋 *{}*: `{}` #{}\n_{}_\n_🔍 即将自动开始审查..._",
                type_label, item.repo, item.pr_number, item.title
            );
            match slack.post_top_message(&msg).await {
                Ok(ts) => {
                    plog!("[Pawkit] PR notification posted, thread_ts={}", ts);
                    thread_ts = Some(ts);
                }
                Err(e) => {
                    plog!("[Pawkit] Failed to post PR notification: {}", e);
                }
            }
        }
    }

    // Always emit to frontend (in case user switches back to home mode)
    let _ = app_handle.emit("review_item_found", item);
    thread_ts
}

/// Post Claude Code output to Slack (thread-aware)
async fn post_review_result_to_slack(
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
    item: &ReviewItem,
    output: &ClaudeOutput,
) {
    if !is_away.load(Ordering::SeqCst) {
        return;
    }
    let Some(ref slack) = slack else { return };

    let type_label = if item.is_own_pr { "PR Feedback" } else { "Review" };
    let header = format!("✅ *{} complete*: `{}` #{}", type_label, item.repo, item.pr_number);
    let _ = review_reply(slack, &item.slack_thread_ts, &header).await;

    // Post the output (truncated)
    let text = &output.text;
    if !text.is_empty() {
        let truncated: String = text.chars().take(2000).collect();
        let _ = review_reply(slack, &item.slack_thread_ts, &truncated).await;
    }

    if let Some(cost) = output.cost_usd {
        if let Some(dur) = output.duration_ms {
            let _ = review_reply(slack, &item.slack_thread_ts, &format!("💰 ${:.4} ⏱ {:.1}s", cost, dur as f64 / 1000.0)).await;
        }
    }
}

async fn post_review_error_to_slack(
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
    item: &ReviewItem,
    error: &str,
) {
    if !is_away.load(Ordering::SeqCst) {
        return;
    }
    let Some(ref slack) = slack else { return };

    let _ = review_reply(slack, &item.slack_thread_ts, &format!(
        "❌ Review failed: `{}` #{}\n```{}```",
        item.repo, item.pr_number, error
    )).await;
}

/// Helper: reply in the review's Slack thread if available, otherwise use active thread
async fn review_reply(slack: &SlackBridge, thread_ts: &Option<String>, msg: &str) -> Result<String, String> {
    match thread_ts {
        Some(ts) => slack.reply_in_thread(ts, msg).await,
        None => slack.reply(msg).await,
    }
}

/// Poll GitHub notifications for PR-related items.
async fn poll_github(
    client: &reqwest::Client,
    app_handle: &tauri::AppHandle,
    config: &AutoReviewConfig,
    seen: &SeenItems,
    pending: &PendingReviewItems,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
    username: &str,
) -> Result<(), String> {
    plog!("[Pawkit] Polling GitHub notifications...");

    // Only fetch unread notifications. GitHub automatically marks notifications as
    // unread again when there's new activity, so we don't miss updates.
    // Using all=true would incorrectly re-surface already-read notifications.
    let since = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    let resp = client
        .get("https://api.github.com/notifications")
        .query(&[
            ("participating", "true"),
            ("since", since.as_str()),
            ("per_page", "50"),
        ])
        .send().await
        .map_err(|e| format!("GitHub notifications API failed: {}", e))?;

    let notifications: Vec<serde_json::Value> = resp.json().await
        .map_err(|e| format!("Failed to parse notifications: {}", e))?;

    plog!("[Pawkit] Received {} notifications", notifications.len());

    // Prune seen set to prevent unbounded growth — keep at most 500 entries.
    // Oldest entries are implicitly stale since we only poll the last 24h.
    {
        let mut seen_lock = seen.lock().await;
        if seen_lock.len() > 500 {
            plog!("[Pawkit] Pruning seen set ({} entries)", seen_lock.len());
            seen_lock.clear();
        }
    }

    for notif in notifications {
        // Only handle PR-related notifications
        let subject_type = notif["subject"]["type"].as_str().unwrap_or("");
        if subject_type != "PullRequest" {
            continue;
        }

        let notif_id = notif["id"].as_str().unwrap_or("").to_string();
        let repo = notif["repository"]["full_name"].as_str().unwrap_or("").to_string();
        let reason = notif["reason"].as_str().unwrap_or("");
        let updated_at = notif["updated_at"].as_str().unwrap_or("");

        // Filter by configured repos (empty = all repos)
        if !config.repos.is_empty() && !config.repos.iter().any(|r| r == &repo) {
            continue;
        }

        // Include updated_at so the same thread is re-processed when new activity arrives
        let id = format!("pr_{}_{}_{}", repo, notif_id, updated_at);

        if seen.lock().await.contains(&id) { continue; }

        // Extract PR number from subject URL (e.g. .../pulls/123)
        let api_url = notif["subject"]["url"].as_str().unwrap_or("");
        let pr_number = api_url.rsplit('/').next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        if pr_number == 0 || repo.is_empty() { continue; }

        // Skip if this PR is already pending
        {
            let pending_lock = pending.lock().await;
            if pending_lock.iter().any(|i| i.repo == repo && i.pr_number == pr_number) {
                continue;
            }
        }

        // Fetch PR data to get creator and state
        let pr_url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
        let pr_data = match client.get(&pr_url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(data) => data,
                Err(e) => {
                    plog!("[Pawkit] Failed to parse PR data for {} #{}: {}", repo, pr_number, e);
                    continue;
                }
            },
            Err(e) => {
                plog!("[Pawkit] Failed to fetch PR data for {} #{}: {}", repo, pr_number, e);
                continue;
            }
        };

        // Skip closed/merged PRs — mark notification as read and move on
        let state = pr_data["state"].as_str().unwrap_or("");
        let merged = pr_data["merged"].as_bool().unwrap_or(false);
        if state == "closed" || merged {
            plog!("[Pawkit] Skipping {} #{}: {} — marking as read", repo, pr_number,
                if merged { "merged" } else { "closed" });
            mark_notification_read(client, &notif_id).await;
            seen.lock().await.insert(id);
            continue;
        }

        // Determine if this is our own PR
        let pr_creator = pr_data["user"]["login"].as_str().unwrap_or("");
        let is_own_pr = pr_creator.eq_ignore_ascii_case(username);

        let web_url = format!("https://github.com/{}/pull/{}", repo, pr_number);

        // Fetch latest comment for context display
        let comment_body = match reason {
            "mention" | "comment" | "subscribed" => {
                fetch_latest_mention_comment(client, &repo, pr_number).await
            }
            _ => String::new(),
        };

        let item_type = match reason {
            "review_requested" => "review_request",
            "mention" => "mention",
            _ => "comment",
        };

        let mut item = ReviewItem {
            id,
            repo: repo.clone(),
            pr_number,
            title: notif["subject"]["title"].as_str().unwrap_or("").to_string(),
            url: web_url,
            item_type: item_type.to_string(),
            body: comment_body,
            is_own_pr,
            notification_id: notif_id,
            slack_thread_ts: None,
        };

        plog!("[Pawkit] Found PR notification: {} #{} (own={}, reason={})",
            repo, pr_number, is_own_pr, reason);
        seen.lock().await.insert(item.id.clone());
        let thread_ts = notify_review_item(app_handle, &item, slack, is_away).await;

        if is_away.load(Ordering::SeqCst) {
            // In away mode: auto-approve and start review immediately
            item.slack_thread_ts = thread_ts;
            if let Some(tx) = get_approved_sender() {
                plog!("[Pawkit] Auto-approving review in away mode: {} #{}", item.repo, item.pr_number);
                let _ = tx.send(item).await;
            }
        } else {
            // In home mode: add to pending for UI approval
            pending.lock().await.push(item);
        }
    }

    Ok(())
}

/// Fetch the latest comment on a PR/issue
async fn fetch_latest_mention_comment(client: &reqwest::Client, repo: &str, pr_number: u64) -> String {
    if pr_number == 0 || repo.is_empty() { return String::new(); }

    let url = format!(
        "https://api.github.com/repos/{}/issues/{}/comments?per_page=5&direction=desc",
        repo, pr_number
    );
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    let comments: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    comments.first()
        .and_then(|c| c["body"].as_str())
        .unwrap_or("")
        .to_string()
}

/// Mark a notification thread as read
async fn mark_notification_read(client: &reqwest::Client, thread_id: &str) {
    let url = format!("https://api.github.com/notifications/threads/{}", thread_id);
    if let Err(e) = client.patch(&url).send().await {
        plog!("[Pawkit] Failed to mark notification {} as read: {}", thread_id, e);
    }
}

/// Lazily-initialized GitHub client for use outside the poll loop (e.g. skip_review_item).
/// Created once, reused for all subsequent calls.
static SHARED_CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();

async fn get_shared_client() -> Result<&'static reqwest::Client, String> {
    SHARED_CLIENT.get_or_try_init(|| async {
        let token = get_github_token(&None).await?;
        Ok(github_client(&token))
    }).await
}

/// Mark a notification as read by thread ID (public, reuses a shared client)
pub async fn mark_notification_read_by_id(thread_id: &str) {
    let client = match get_shared_client().await {
        Ok(c) => c,
        Err(_) => return,
    };
    mark_notification_read(client, thread_id).await;
}

/// Try to merge a PR if it's in a mergeable state (all checks pass, approved, no conflicts).
/// Returns Ok(true) if merged, Ok(false) if not mergeable, Err on API failure.
async fn try_merge_pr(
    client: &reqwest::Client,
    repo: &str,
    pr_number: u64,
) -> Result<bool, String> {
    let url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
    let resp = client.get(&url).send().await
        .map_err(|e| format!("Failed to fetch PR: {}", e))?;
    let pr: serde_json::Value = resp.json().await
        .map_err(|e| format!("Failed to parse PR: {}", e))?;

    let mergeable = pr["mergeable"].as_bool();
    let state = pr["mergeable_state"].as_str().unwrap_or("");

    if mergeable != Some(true) || state != "clean" {
        plog!("[Pawkit] PR {} #{} not mergeable (mergeable={:?}, state={})",
            repo, pr_number, mergeable, state);
        return Ok(false);
    }

    let title = pr["title"].as_str().unwrap_or("");
    let merge_url = format!("https://api.github.com/repos/{}/pulls/{}/merge", repo, pr_number);
    let resp = client.put(&merge_url)
        .json(&serde_json::json!({
            "commit_title": format!("{} (#{})", title, pr_number),
            "merge_method": "squash"
        }))
        .send().await
        .map_err(|e| format!("Merge API failed: {}", e))?;

    let status = resp.status();
    if status.is_success() {
        Ok(true)
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Merge failed ({}): {}", status, body))
    }
}

/// Process approved review items using Claude Code
async fn process_approved_items(
    mut rx: ApprovedRx,
    config: &AutoReviewConfig,
    app_handle: &tauri::AppHandle,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
    client: &reqwest::Client,
    session_thread_map: SessionThreadMap,
) {
    while let Some(item) = rx.recv().await {
        plog!("[Pawkit] Processing approved review item: {} #{}", item.repo, item.pr_number);

        // Set active thread to this review's Slack thread (for auth button routing)
        if let Some(ref ts) = item.slack_thread_ts {
            if let Some(ref s) = slack {
                s.set_active_thread(ts).await;
            }
        }

        // Notify Slack that we're starting
        if is_away.load(Ordering::SeqCst) {
            if let Some(ref s) = slack {
                let _ = review_reply(s, &item.slack_thread_ts, "🔍 _开始审查..._").await;
                if let Some(ref ts) = item.slack_thread_ts {
                    let _ = s.set_status_in_thread(ts, "🔍 Reviewing PR...").await;
                } else {
                    let _ = s.set_status("🔍 Reviewing PR...").await;
                }
            }
        }

        let working_dir = config.repo_dirs.get(&item.repo)
            .cloned()
            .unwrap_or_else(|| "E:\\develop\\code".to_string());

        let prompt = build_review_prompt(&item);

        let mut session = ClaudeSession::new(working_dir);
        match session.run_prompt(&prompt).await {
            Ok(output) => {
                plog!("[Pawkit] Review complete for {} #{}: {}",
                    item.repo, item.pr_number,
                    &output.text[..output.text.len().min(200)]);

                // Register review session in thread map so hooks route correctly
                if let Some(ref sid) = output.session_id {
                    if let Some(ref ts) = item.slack_thread_ts {
                        session_thread_map.lock().await.insert(sid.clone(), ts.clone());
                        plog!("[Pawkit] Registered review session {} → thread {}", sid, ts);
                    }
                }

                // Post result to Slack in away mode
                post_review_result_to_slack(slack, is_away, &item, &output).await;

                // Try to auto-merge only if enabled in config (default: off)
                let merged = if !config.auto_merge {
                    false
                } else {
                    match try_merge_pr(client, &item.repo, item.pr_number).await {
                        Ok(true) => {
                            plog!("[Pawkit] Auto-merged {} #{}", item.repo, item.pr_number);
                            if is_away.load(Ordering::SeqCst) {
                                if let Some(ref s) = slack {
                                    let _ = review_reply(s, &item.slack_thread_ts, &format!(
                                        "🎉 *Auto-merged*: `{}` #{}", item.repo, item.pr_number
                                    )).await;
                                }
                            }
                            true
                        }
                        Ok(false) => false,
                        Err(e) => {
                            plog!("[Pawkit] Auto-merge failed for {} #{}: {}", item.repo, item.pr_number, e);
                            false
                        }
                    }
                };

                // Mark notification as read only on success
                if !item.notification_id.is_empty() {
                    mark_notification_read(client, &item.notification_id).await;
                }

                let _ = app_handle.emit("review_item_done", &serde_json::json!({
                    "id": item.id,
                    "merged": merged,
                }));
            }
            Err(e) => {
                plog!("[Pawkit] Review failed for {} #{}: {}", item.repo, item.pr_number, e);

                // Post error to Slack in away mode
                post_review_error_to_slack(slack, is_away, &item, &e).await;

                let _ = app_handle.emit("review_item_error", &serde_json::json!({
                    "id": item.id,
                    "error": e,
                }));
            }
        }

        // Clear Slack status
        if is_away.load(Ordering::SeqCst) {
            if let Some(ref s) = slack {
                if let Some(ref ts) = item.slack_thread_ts {
                    let _ = s.set_status_in_thread(ts, "").await;
                } else {
                    let _ = s.clear_status().await;
                }
            }
        }
    }
}

fn build_review_prompt(item: &ReviewItem) -> String {
    if item.is_own_pr {
        format!("/pr-creator-feedback {}", item.url)
    } else {
        format!("/pr-reviewer {}", item.url)
    }
}
