use crate::plog;
use serde::Serialize;
use tokio::process::Command;
use std::process::Stdio;


pub struct ClaudeSession {
    session_id: Option<String>,
    /// Use --continue on first call (to inherit the local session)
    use_continue: bool,
    working_dir: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaudeOutput {
    pub text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
}

impl ClaudeSession {
    pub fn new(working_dir: String) -> Self {
        Self {
            session_id: None,
            use_continue: false,
            working_dir,
        }
    }

    /// Create a session that uses --continue on first call
    /// to inherit the most recent local Claude Code session
    pub fn new_continue(working_dir: String) -> Self {
        Self {
            session_id: None,
            use_continue: true,
            working_dir,
        }
    }

    /// Create a session that resumes a specific session by ID
    pub fn new_resume(session_id: String, working_dir: String) -> Self {
        Self {
            session_id: Some(session_id),
            use_continue: false,
            working_dir,
        }
    }


    pub fn working_dir(&self) -> &str {
        &self.working_dir
    }

    pub fn set_working_dir(&mut self, dir: String) {
        self.working_dir = dir;
    }

    pub fn reset(&mut self) {
        self.session_id = None;
    }

    /// Like `run_prompt`, but calls `on_pid` with the child process PID right after spawn.
    /// Used by LLM title gen to register the PID as internal (so hooks from it are ignored).
    pub async fn run_prompt_tracked(
        &mut self,
        prompt: &str,
        on_pid: impl FnOnce(u32),
    ) -> Result<ClaudeOutput, String> {
        self.run_prompt_inner(prompt, Some(on_pid)).await
    }

    pub async fn run_prompt(&mut self, prompt: &str) -> Result<ClaudeOutput, String> {
        self.run_prompt_inner(prompt, None::<fn(u32)>).await
    }

    async fn run_prompt_inner(
        &mut self,
        prompt: &str,
        on_pid: Option<impl FnOnce(u32)>,
    ) -> Result<ClaudeOutput, String> {
        plog!("[Pawkit] run_prompt: resume={:?} dir={} prompt_len={}", self.session_id.as_deref(), self.working_dir, prompt.len());

        // Write prompt to temp file, then pipe via `type file | claude`.
        // This avoids cmd.exe mangling special characters (*, `, ", ^, |)
        // and Windows command line length limits (~32K chars).
        let temp_file = std::env::temp_dir().join(format!("pawkit_prompt_{}.txt", std::process::id()));
        std::fs::write(&temp_file, prompt).map_err(|e| format!("写入临时文件失败: {}", e))?;

        let mut cmd = self.build_command_piped(&temp_file);

        let child = cmd
            .spawn()
            .map_err(|e| format!("启动 claude 失败: {}", e))?;

        // Notify caller of the child PID (for internal process tracking)
        if let Some(cb) = on_pid {
            if let Some(pid) = child.id() {
                cb(pid);
            }
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| format!("claude 进程错误: {}", e))?;

        let _ = std::fs::remove_file(&temp_file);

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() && stdout.trim().is_empty() {
            return Err(format!("claude 退出错误: {}", stderr));
        }

        // The JSON output may be preceded by log lines on stderr; stdout is the JSON result
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Err(format!("claude 无输出。stderr: {}", stderr));
        }

        // Parse the JSON result — find the last JSON object in stdout
        let json_str = find_last_json_object(trimmed).unwrap_or(trimmed);

        match serde_json::from_str::<serde_json::Value>(json_str) {
            Ok(json) => {
                let result_text = json
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let session_id = json
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let cost = json.get("cost_usd").and_then(|v| v.as_f64());
                let is_error = json
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let duration = json.get("duration_ms").and_then(|v| v.as_u64());

                // Save session ID for --resume on next call
                if let Some(ref sid) = session_id {
                    plog!("[Pawkit] Claude session_id saved: {}", sid);
                    self.session_id = Some(sid.clone());
                    self.use_continue = false;
                } else {
                    plog!("[Pawkit] WARNING: no session_id in Claude output");
                    plog!("[Pawkit] JSON keys: {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>()));
                }

                Ok(ClaudeOutput {
                    text: result_text,
                    session_id,
                    cost_usd: cost,
                    is_error,
                    duration_ms: duration,
                })
            }
            Err(_) => {
                // JSON parsing failed — treat raw output as plain text result.
                // This happens when --output-format json doesn't take effect.
                plog!("[Pawkit] WARNING: Claude output is not JSON, using raw text");
                Ok(ClaudeOutput {
                    text: trimmed.to_string(),
                    session_id: None,
                    cost_usd: None,
                    is_error: false,
                    duration_ms: None,
                })
            }
        }
    }

    /// Build command that reads prompt from a temp file via shell.
    /// Uses `claude -p "$(cat file)"`, avoiding shell escaping issues.
    fn build_command_piped(&self, file_path: &std::path::Path) -> Command {
        let file_str = file_path.to_string_lossy();

        #[cfg(target_os = "windows")]
        let (shell, unix_path) = {
            let bash_path = Self::find_git_bash();
            // Convert Windows path to Unix path for bash (C:\Users\... → /c/Users/...)
            let fstr = file_str.replace('\\', "/");
            let upath = if fstr.len() >= 2 && fstr.as_bytes()[1] == b':' {
                format!("/{}/{}", &fstr[0..1].to_lowercase(), &fstr[3..])
            } else {
                fstr
            };
            (bash_path, upath)
        };

        #[cfg(not(target_os = "windows"))]
        let (shell, unix_path) = {
            ("/bin/bash".to_string(), file_str.to_string())
        };

        let mut shell_cmd = format!(
            "claude -p \"$(cat '{}')\" --output-format json",
            unix_path
        );

        if let Some(ref sid) = self.session_id {
            shell_cmd.push_str(&format!(" --resume {}", sid));
        } else if self.use_continue {
            shell_cmd.push_str(" --continue");
        }

        let mut cmd = Command::new(&shell);
        cmd.arg("-c").arg(&shell_cmd);
        cmd.current_dir(&self.working_dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(target_os = "windows")]
        {
            cmd.env("CLAUDE_CODE_GIT_BASH_PATH", &shell);
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        cmd
    }

    #[cfg(target_os = "windows")]
    fn find_git_bash() -> String {
        if let Ok(path) = std::env::var("CLAUDE_CODE_GIT_BASH_PATH") {
            return path;
        }
        let candidates = [
            r"E:\develop\kit\Git\bin\bash.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for path in &candidates {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }
        "bash".to_string()
    }
}

/// Find the last complete JSON object in a string (handles prefixed log lines)
fn find_last_json_object(s: &str) -> Option<&str> {
    // Look for the last '{' that starts a valid JSON
    let mut depth = 0;
    let mut start = None;
    let bytes = s.as_bytes();

    // Scan from the end to find the outermost JSON object
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b'}' => {
                if depth == 0 {
                    // This is the end of the last JSON object
                    // Now find the matching opening brace
                    let end = i + 1;
                    let mut d = 0;
                    let mut in_string = false;
                    let mut escape = false;
                    for j in (0..end).rev() {
                        if escape {
                            escape = false;
                            continue;
                        }
                        match bytes[j] {
                            b'\\' if in_string => escape = true,
                            b'"' => in_string = !in_string,
                            b'}' if !in_string => d += 1,
                            b'{' if !in_string => {
                                d -= 1;
                                if d == 0 {
                                    start = Some(j);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(s_idx) = start {
                        return Some(&s[s_idx..end]);
                    }
                }
                depth += 1;
            }
            b'{' => {
                depth -= 1;
            }
            _ => {}
        }
        if start.is_some() {
            break;
        }
    }

    None
}
