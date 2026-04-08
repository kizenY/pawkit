use crate::plog;
use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::AutoReviewConfig;
use crate::slack_bridge::SlackBridge;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex};

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

/// A review item detected by the polling loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: String,
    pub repo: String,
    pub pr_number: u64,
    pub title: String,
    pub url: String,
    pub item_type: String, // "review_request" or "mention"
    pub body: String,      // comment body for mentions, empty for review requests
}

/// Tracks which items we've already notified about
type SeenItems = Arc<Mutex<HashSet<String>>>;

/// Channel for approved items waiting to be processed
type ApprovedTx = mpsc::Sender<ReviewItem>;
type ApprovedRx = mpsc::Receiver<ReviewItem>;

/// Pending review items waiting for user decision
pub type PendingReviewItems = Arc<Mutex<Vec<ReviewItem>>>;

/// Start the auto-review background system
pub fn start_auto_review(
    app_handle: tauri::AppHandle,
    config: AutoReviewConfig,
    pending_items: PendingReviewItems,
    slack: Option<Arc<SlackBridge>>,
    is_away: Arc<AtomicBool>,
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

        // Task 1: Poll GitHub for review requests and mentions
        tauri::async_runtime::spawn(async move {
            let interval = Duration::from_secs(poll_config.interval_minutes * 60);
            // Initial short delay before first poll
            tokio::time::sleep(Duration::from_secs(10)).await;

            loop {
                if let Err(e) = poll_github(
                    &client, &poll_handle, &poll_config, &poll_seen, &poll_pending,
                    &poll_slack, &poll_away,
                ).await {
                    plog!("[Pawkit] Auto-review poll error: {}", e);
                }
                tokio::time::sleep(interval).await;
            }
        });

        // Task 2: Process approved items with Claude Code
        process_approved_items(approved_rx, &proc_config, &proc_handle, &proc_slack, &proc_away, &client2).await;
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

/// Notify about a review item — via Slack in away mode, via cat UI in home mode
async fn notify_review_item(
    app_handle: &tauri::AppHandle,
    item: &ReviewItem,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
) {
    let type_label = match item.item_type.as_str() {
        "review_request" => "Review Request",
        "mention" => "@Mention",
        "comment" => "New Comment",
        _ => "Notification",
    };

    if is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = slack {
            let msg = format!(
                "📋 *{}*: `{}` #{}\n_{}_",
                type_label, item.repo, item.pr_number, item.title
            );
            let _ = slack.post_top_message(&msg).await;
        }
    }

    // Always emit to frontend (in case user switches back to home mode)
    let _ = app_handle.emit("review_item_found", item);
}

/// Post Claude Code output to Slack
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

    let type_label = if item.item_type == "review_request" { "Review" } else { "Reply" };
    let header = format!("✅ *{} complete*: `{}` #{}", type_label, item.repo, item.pr_number);
    let _ = slack.reply(&header).await;

    // Post the output (truncated)
    let text = &output.text;
    if !text.is_empty() {
        let truncated: String = text.chars().take(2000).collect();
        let _ = slack.reply(&truncated).await;
    }

    if let Some(cost) = output.cost_usd {
        if let Some(dur) = output.duration_ms {
            let _ = slack.reply(&format!("💰 ${:.4} ⏱ {:.1}s", cost, dur as f64 / 1000.0)).await;
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

    let _ = slack.reply(&format!(
        "❌ Review failed: `{}` #{}\n```{}```",
        item.repo, item.pr_number, error
    )).await;
}

/// Poll GitHub for review requests and mentions using the REST API directly.
async fn poll_github(
    client: &reqwest::Client,
    app_handle: &tauri::AppHandle,
    config: &AutoReviewConfig,
    seen: &SeenItems,
    pending: &PendingReviewItems,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
) -> Result<(), String> {
    plog!("[Pawkit] Polling GitHub for review items...");

    // 1. Check review requests
    if config.repos.is_empty() {
        // No repo filter: search all PRs requesting review from @me
        let resp = client
            .get("https://api.github.com/search/issues")
            .query(&[
                ("q", "is:pr is:open review-requested:@me"),
                ("per_page", "20"),
            ])
            .send().await
            .map_err(|e| format!("GitHub search API failed: {}", e))?;

        let body: serde_json::Value = resp.json().await
            .map_err(|e| format!("Failed to parse search response: {}", e))?;

        if let Some(items) = body["items"].as_array() {
            for item in items {
                let number = item["number"].as_u64().unwrap_or(0);
                // Extract repo from repository_url: https://api.github.com/repos/OWNER/REPO
                let repo = item["repository_url"].as_str().unwrap_or("")
                    .strip_prefix("https://api.github.com/repos/")
                    .unwrap_or("").to_string();
                if repo.is_empty() { continue; }
                let id = format!("review_{}_{}", repo, number);

                let mut seen_lock = seen.lock().await;
                if seen_lock.contains(&id) { continue; }
                seen_lock.insert(id.clone());
                drop(seen_lock);

                let review_item = ReviewItem {
                    id,
                    repo: repo.clone(),
                    pr_number: number,
                    title: item["title"].as_str().unwrap_or("").to_string(),
                    url: item["html_url"].as_str().unwrap_or("").to_string(),
                    item_type: "review_request".to_string(),
                    body: String::new(),
                };

                plog!("[Pawkit] Found review request: {} #{}", repo, number);
                pending.lock().await.push(review_item.clone());
                notify_review_item(app_handle, &review_item, slack, is_away).await;
            }
        }
    } else {
        // Specific repos: search PRs requesting review from @me per repo
        for repo in &config.repos {
            let query = format!("is:pr is:open review-requested:@me repo:{}", repo);
            let resp = client
                .get("https://api.github.com/search/issues")
                .query(&[("q", query.as_str()), ("per_page", "10")])
                .send().await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    plog!("[Pawkit] Failed to search PRs for {}: {}", repo, e);
                    continue;
                }
            };

            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => continue,
            };

            let items = match body["items"].as_array() {
                Some(a) => a,
                None => continue,
            };

            for pr in items {
                let number = pr["number"].as_u64().unwrap_or(0);
                let id = format!("review_{}_{}", repo, number);

                let mut seen_lock = seen.lock().await;
                if seen_lock.contains(&id) { continue; }
                seen_lock.insert(id.clone());
                drop(seen_lock);

                let review_item = ReviewItem {
                    id,
                    repo: repo.clone(),
                    pr_number: number,
                    title: pr["title"].as_str().unwrap_or("").to_string(),
                    url: pr["html_url"].as_str().unwrap_or("").to_string(),
                    item_type: "review_request".to_string(),
                    body: String::new(),
                };

                plog!("[Pawkit] Found review request: {} #{}", repo, number);
                pending.lock().await.push(review_item.clone());
                notify_review_item(app_handle, &review_item, slack, is_away).await;
            }
        }
    }

    // 2. Check notifications for mentions and new comments
    let resp = client
        .get("https://api.github.com/notifications")
        .send().await
        .map_err(|e| format!("GitHub notifications API failed: {}", e))?;

    let notifications: Vec<serde_json::Value> = resp.json().await
        .map_err(|e| format!("Failed to parse notifications: {}", e))?;

    for notif in notifications {
        let reason = notif["reason"].as_str().unwrap_or("");
        // Only handle mention, comment, and subscribed (PR participant) notifications
        let item_type = match reason {
            "mention" => "mention",
            "comment" | "subscribed" => "comment",
            _ => continue,
        };

        let notif_id = notif["id"].as_str().unwrap_or("").to_string();
        let repo = notif["repository"]["full_name"].as_str().unwrap_or("").to_string();
        let id = format!("{}_{}", item_type, notif_id);

        // Only track repos we're configured to watch (skip filter if repos list is empty = all repos)
        if !config.repos.is_empty() && !config.repos.iter().any(|r| r == &repo) {
            continue;
        }

        // Only handle PR-related notifications
        let subject_type = notif["subject"]["type"].as_str().unwrap_or("");
        if subject_type != "PullRequest" {
            continue;
        }

        let mut seen_lock = seen.lock().await;
        if seen_lock.contains(&id) { continue; }
        seen_lock.insert(id.clone());
        drop(seen_lock);

        // Extract PR number from subject URL (e.g. .../pulls/123)
        let api_url = notif["subject"]["url"].as_str().unwrap_or("");
        let pr_number = api_url.rsplit('/').next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // Skip merged/closed PRs
        if pr_number > 0 && !repo.is_empty() {
            let pr_url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
            if let Ok(pr_resp) = client.get(&pr_url).send().await {
                if let Ok(pr_data) = pr_resp.json::<serde_json::Value>().await {
                    let state = pr_data["state"].as_str().unwrap_or("");
                    let merged = pr_data["merged"].as_bool().unwrap_or(false);
                    if state == "closed" || merged {
                        plog!("[Pawkit] Skipping {} #{}: {}", repo, pr_number,
                            if merged { "merged" } else { "closed" });
                        continue;
                    }
                }
            }
        }

        // Convert API URL to web URL
        let web_url = if pr_number > 0 && !repo.is_empty() {
            format!("https://github.com/{}/pull/{}", repo, pr_number)
        } else {
            api_url.replace("api.github.com/repos", "github.com").replace("/pulls/", "/pull/")
        };

        // Fetch the latest comment
        let comment_body = fetch_latest_mention_comment(client, &repo, pr_number).await;

        let item = ReviewItem {
            id,
            repo: repo.clone(),
            pr_number,
            title: notif["subject"]["title"].as_str().unwrap_or("").to_string(),
            url: web_url,
            item_type: item_type.to_string(),
            body: comment_body,
        };

        plog!("[Pawkit] Found {}: {} #{}", item_type, repo, pr_number);
        pending.lock().await.push(item.clone());
        notify_review_item(app_handle, &item, slack, is_away).await;
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

/// Process approved review items using Claude Code
async fn process_approved_items(
    mut rx: ApprovedRx,
    config: &AutoReviewConfig,
    app_handle: &tauri::AppHandle,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
    client: &reqwest::Client,
) {

    while let Some(item) = rx.recv().await {
        plog!("[Pawkit] Processing approved review item: {} #{}", item.repo, item.pr_number);

        // Notify Slack that we're starting
        if is_away.load(Ordering::SeqCst) {
            if let Some(ref s) = slack {
                let _ = s.set_status("🔍 Reviewing PR...").await;
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

                // Post result to Slack in away mode
                post_review_result_to_slack(slack, is_away, &item, &output).await;

                // Mark notification as read
                if item.item_type == "mention" || item.item_type == "comment" {
                    let prefix = format!("{}_", item.item_type);
                    let notif_id = item.id.strip_prefix(&prefix).unwrap_or(&item.id);
                    mark_notification_read(client, notif_id).await;
                }

                let _ = app_handle.emit("review_item_done", &item.id);
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
                let _ = s.clear_status().await;
            }
        }
    }
}

fn build_review_prompt(item: &ReviewItem) -> String {
    match item.item_type.as_str() {
        "review_request" => format!(
            r#"You need to review PR #{number} "{title}" in {repo}.

Steps:
1. Run: gh pr view {number} --repo {repo}   to read the PR description
2. Run: gh pr diff {number} --repo {repo}   to read the code changes
3. Analyze the changes carefully for: bugs, security issues, logic errors, code quality, missing edge cases
4. Submit your review:
   - If changes look good: gh pr review {number} --repo {repo} --approve --body "your review comments"
   - If issues found: gh pr review {number} --repo {repo} --request-changes --body "your detailed feedback"
   - If minor suggestions: gh pr review {number} --repo {repo} --comment --body "your comments"
5. If the PR is approved and ready, merge it: gh pr merge {number} --repo {repo} --squash

Write review comments in English. Be concise but thorough."#,
            number = item.pr_number,
            title = item.title,
            repo = item.repo,
        ),
        "mention" => format!(
            r#"You were @mentioned in PR #{number} "{title}" in {repo}.

The latest comment says:
---
{body}
---

Steps:
1. Run: gh pr view {number} --repo {repo}   to understand the PR context
2. Read the comment above and determine what's needed:
   - If it's a question: reply with an answer using gh pr comment {number} --repo {repo} --body "..."
   - If it asks for a review: review the PR diff and submit a review
   - If it asks for code changes: make the changes locally, commit, and push
   - If the PR is approved and they ask to merge: gh pr merge {number} --repo {repo} --squash
3. Take the appropriate action.

Write responses in English. Be concise."#,
            number = item.pr_number,
            title = item.title,
            repo = item.repo,
            body = if item.body.is_empty() { "(could not fetch comment)" } else { &item.body },
        ),
        "comment" => format!(
            r#"There is a new comment on PR #{number} "{title}" in {repo}.

The latest comment says:
---
{body}
---

Steps:
1. Run: gh pr view {number} --repo {repo}   to understand the PR context
2. Run: gh pr view {number} --repo {repo} --comments   to read the full comment thread
3. Based on the comment and context, decide if this needs YOUR action:
   - If the comment is asking you (the reviewer) for feedback, a re-review, or a response → take action
   - If the comment is a follow-up to your previous review → take action
   - If the comment is clearly directed at someone else or is just an FYI → do nothing
4. If action is needed:
   - To reply: gh pr comment {number} --repo {repo} --body "..."
   - To re-review: review the PR diff and submit a review
   - To approve after changes: gh pr review {number} --repo {repo} --approve --body "..."
5. If no action is needed, just say "No action needed" and explain briefly why.

Write responses in English. Be concise."#,
            number = item.pr_number,
            title = item.title,
            repo = item.repo,
            body = if item.body.is_empty() { "(could not fetch comment)" } else { &item.body },
        ),
        _ => String::new(),
    }
}
