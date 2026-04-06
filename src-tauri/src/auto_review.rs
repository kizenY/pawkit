use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::AutoReviewConfig;
use crate::slack_bridge::SlackBridge;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::{mpsc, Mutex};

/// Create a Command that runs `gh` through the platform shell.
fn gh_command(args: &[&str]) -> tokio::process::Command {
    if cfg!(target_os = "windows") {
        let mut cmd = tokio::process::Command::new("cmd");
        let mut all = vec!["/C", "gh"];
        all.extend_from_slice(args);
        cmd.args(all);
        cmd
    } else {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        let escaped: Vec<String> = args.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect();
        cmd.args(["-c", &format!("gh {}", escaped.join(" "))]);
        cmd
    }
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
    if !config.enabled || config.repos.is_empty() {
        println!("[Pawkit] Auto-review disabled or no repos configured");
        return;
    }

    let (approved_tx, approved_rx) = mpsc::channel::<ReviewItem>(32);
    let seen: SeenItems = Arc::new(Mutex::new(HashSet::new()));

    // Task 1: Poll GitHub for review requests and mentions
    let poll_handle = app_handle.clone();
    let poll_config = config.clone();
    let poll_seen = seen.clone();
    let poll_pending = pending_items.clone();
    let poll_slack = slack.clone();
    let poll_away = is_away.clone();
    tauri::async_runtime::spawn(async move {
        let interval = Duration::from_secs(poll_config.interval_minutes * 60);
        // Initial short delay before first poll
        tokio::time::sleep(Duration::from_secs(10)).await;

        loop {
            if let Err(e) = poll_github(
                &poll_handle, &poll_config, &poll_seen, &poll_pending,
                &poll_slack, &poll_away,
            ).await {
                eprintln!("[Pawkit] Auto-review poll error: {}", e);
            }
            tokio::time::sleep(interval).await;
        }
    });

    // Task 2: Process approved items with Claude Code
    let proc_config = config.clone();
    let proc_handle = app_handle.clone();
    let proc_slack = slack.clone();
    let proc_away = is_away.clone();
    tauri::async_runtime::spawn(async move {
        process_approved_items(approved_rx, &proc_config, &proc_handle, &proc_slack, &proc_away).await;
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
    let type_label = if item.item_type == "review_request" { "Review Request" } else { "@Mention" };

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

/// Poll GitHub for review requests and mentions
async fn poll_github(
    app_handle: &tauri::AppHandle,
    config: &AutoReviewConfig,
    seen: &SeenItems,
    pending: &PendingReviewItems,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
) -> Result<(), String> {
    println!("[Pawkit] Polling GitHub for review items...");

    // Switch gh account if configured
    if let Some(ref account) = config.gh_account {
        let _ = gh_command(&["auth", "switch", "-u", account])
            .output()
            .await;
    }

    // 1. Check review requests for each repo
    for repo in &config.repos {
        let output = gh_command(&["pr", "list",
                "--repo", repo,
                "--search", "review-requested:@me",
                "--json", "number,title,url",
                "--limit", "10"])
            .output()
            .await
            .map_err(|e| format!("gh pr list failed: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(stdout.trim()) {
            for pr in prs {
                let number = pr["number"].as_u64().unwrap_or(0);
                let id = format!("review_{}_{}", repo, number);

                let mut seen_lock = seen.lock().await;
                if seen_lock.contains(&id) {
                    continue;
                }
                seen_lock.insert(id.clone());
                drop(seen_lock);

                let item = ReviewItem {
                    id,
                    repo: repo.clone(),
                    pr_number: number,
                    title: pr["title"].as_str().unwrap_or("").to_string(),
                    url: pr["url"].as_str().unwrap_or("").to_string(),
                    item_type: "review_request".to_string(),
                    body: String::new(),
                };

                println!("[Pawkit] Found review request: {} #{}", repo, number);
                pending.lock().await.push(item.clone());
                notify_review_item(app_handle, &item, slack, is_away).await;
            }
        }
    }

    // 2. Check notifications for mentions
    let output = gh_command(&["api", "notifications",
            "--jq", "[.[] | select(.reason == \"mention\") | {id: .id, reason: .reason, title: .subject.title, url: .subject.url, repo: .repository.full_name}]"])
        .output()
        .await
        .map_err(|e| format!("gh api notifications failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(notifications) = serde_json::from_str::<Vec<serde_json::Value>>(stdout.trim()) {
        for notif in notifications {
            let notif_id = notif["id"].as_str().unwrap_or("").to_string();
            let repo = notif["repo"].as_str().unwrap_or("").to_string();
            let id = format!("mention_{}", notif_id);

            // Only track repos we're configured to watch
            if !config.repos.iter().any(|r| r == &repo) {
                continue;
            }

            let mut seen_lock = seen.lock().await;
            if seen_lock.contains(&id) {
                continue;
            }
            seen_lock.insert(id.clone());
            drop(seen_lock);

            // Extract PR number from URL (e.g. .../pulls/123)
            let api_url = notif["url"].as_str().unwrap_or("");
            let pr_number = api_url.rsplit('/').next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            // Skip merged/closed PRs
            if pr_number > 0 {
                let state_output = gh_command(&["pr", "view",
                        &pr_number.to_string(), "--repo", &repo,
                        "--json", "state", "--jq", ".state"])
                    .output()
                    .await;
                if let Ok(o) = state_output {
                    let state = String::from_utf8_lossy(&o.stdout).trim().to_lowercase();
                    if state == "merged" || state == "closed" {
                        println!("[Pawkit] Skipping {} #{}: {}", repo, pr_number, state);
                        continue;
                    }
                }
            }

            // Convert API URL to web URL
            // api.github.com/repos/OWNER/REPO/pulls/N → github.com/OWNER/REPO/pull/N
            let web_url = if pr_number > 0 {
                format!("https://github.com/{}/pull/{}", repo, pr_number)
            } else {
                api_url.replace("api.github.com/repos", "github.com").replace("/pulls/", "/pull/")
            };

            // Fetch the comment details
            let comment_body = fetch_latest_mention_comment(&repo, pr_number).await;

            let item = ReviewItem {
                id,
                repo: repo.clone(),
                pr_number,
                title: notif["title"].as_str().unwrap_or("").to_string(),
                url: web_url,
                item_type: "mention".to_string(),
                body: comment_body,
            };

            println!("[Pawkit] Found mention: {} #{}", repo, pr_number);
            pending.lock().await.push(item.clone());
            notify_review_item(app_handle, &item, slack, is_away).await;
        }
    }

    Ok(())
}

/// Fetch the latest comment mentioning the user on a PR
async fn fetch_latest_mention_comment(repo: &str, pr_number: u64) -> String {
    if pr_number == 0 { return String::new(); }

    let endpoint = format!("repos/{}/issues/{}/comments?per_page=5&direction=desc", repo, pr_number);
    let output = gh_command(&["api", &endpoint,
            "--jq", ".[0].body // \"\""])
        .output()
        .await;

    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Process approved review items using Claude Code
async fn process_approved_items(
    mut rx: ApprovedRx,
    config: &AutoReviewConfig,
    app_handle: &tauri::AppHandle,
    slack: &Option<Arc<SlackBridge>>,
    is_away: &Arc<AtomicBool>,
) {
    while let Some(item) = rx.recv().await {
        println!("[Pawkit] Processing approved review item: {} #{}", item.repo, item.pr_number);

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
                println!("[Pawkit] Review complete for {} #{}: {}",
                    item.repo, item.pr_number,
                    &output.text[..output.text.len().min(200)]);

                // Post result to Slack in away mode
                post_review_result_to_slack(slack, is_away, &item, &output).await;

                // Mark notification as read
                if item.item_type == "mention" {
                    let notif_id = item.id.strip_prefix("mention_").unwrap_or(&item.id);
                    let _ = gh_command(&["api", "-X", "PATCH",
                            &format!("notifications/threads/{}", notif_id)])
                        .output()
                        .await;
                }

                let _ = app_handle.emit("review_item_done", &item.id);
            }
            Err(e) => {
                eprintln!("[Pawkit] Review failed for {} #{}: {}", item.repo, item.pr_number, e);

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
        _ => String::new(),
    }
}
