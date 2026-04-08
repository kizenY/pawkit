use crate::config::Action;
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub action_id: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
}

pub fn execute_action(action: &Action) -> ActionResult {
    let start = std::time::Instant::now();

    let result = match action.action_type.as_str() {
        "meow" => Ok(("meow!".to_string(), String::new(), Some(0))),
        "shell" => execute_shell(action),
        "script" => execute_script(action),
        "url" => execute_url(action),
        "http" => execute_http(action),
        "pipeline" => execute_pipeline(action),
        "claude" => execute_claude(action),
        _ => Err(format!("Unknown action type: {}", action.action_type)),
    };

    let duration_ms = start.elapsed().as_millis();

    match result {
        Ok((stdout, stderr, exit_code)) => ActionResult {
            action_id: action.id.clone(),
            success: exit_code.map_or(true, |c| c == 0),
            stdout,
            stderr,
            exit_code,
            duration_ms,
        },
        Err(err) => ActionResult {
            action_id: action.id.clone(),
            success: false,
            stdout: String::new(),
            stderr: err,
            exit_code: None,
            duration_ms,
        },
    }
}

type ExecResult = Result<(String, String, Option<i32>), String>;

fn execute_shell(action: &Action) -> ExecResult {
    let command = action.command.as_deref().ok_or("Missing 'command' field")?;

    let mut cmd = if cfg!(target_os = "windows") {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/C {}", command));
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", command]);
        c
    };

    if let Some(workdir) = &action.workdir {
        cmd.current_dir(workdir);
    }

    if let Some(env) = &action.env {
        for (key, value) in env {
            cmd.env(key, resolve_env_vars(value));
        }
    }

    run_command(cmd)
}

fn execute_script(action: &Action) -> ExecResult {
    let path = action.path.as_deref().ok_or("Missing 'path' field")?;

    let (program, base_args) = if path.ends_with(".ps1") {
        ("powershell", vec!["-ExecutionPolicy", "Bypass", "-File", path])
    } else if path.ends_with(".py") {
        ("python", vec![path])
    } else if path.ends_with(".sh") {
        ("bash", vec![path])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", path])
    } else {
        ("sh", vec![path])
    };

    let mut cmd = Command::new(program);
    cmd.args(&base_args);

    if let Some(args) = &action.args {
        cmd.args(args);
    }

    if let Some(workdir) = &action.workdir {
        cmd.current_dir(workdir);
    }

    run_command(cmd)
}

fn execute_url(action: &Action) -> ExecResult {
    let url = action.url.as_deref().ok_or("Missing 'url' field")?;
    open::that(url).map_err(|e| format!("Failed to open URL: {}", e))?;
    Ok((format!("Opened: {}", url), String::new(), Some(0)))
}

fn execute_http(action: &Action) -> ExecResult {
    let url = action.url.as_deref().ok_or("Missing 'url' field")?;
    let method = action.method.as_deref().unwrap_or("GET");
    let timeout_secs = action.timeout.unwrap_or(30);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let mut request = match method.to_uppercase().as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        "PATCH" => client.patch(url),
        _ => return Err(format!("Unsupported HTTP method: {}", method)),
    };

    if let Some(headers) = &action.headers {
        for (key, value) in headers {
            request = request.header(key, resolve_env_vars(value));
        }
    }

    if let Some(body) = &action.body {
        request = request.body(resolve_env_vars(body));
    }

    let response = request.send().map_err(|e| format!("HTTP request failed: {}", e))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();

    if status.is_success() {
        Ok((body, String::new(), Some(0)))
    } else {
        Ok((String::new(), body, Some(status.as_u16() as i32)))
    }
}

fn execute_pipeline(action: &Action) -> ExecResult {
    let steps = action.steps.as_ref().ok_or("Missing 'steps' field")?;
    let on_failure = action.on_failure.as_deref().unwrap_or("stop");

    let mut all_stdout = Vec::new();
    let mut all_stderr = Vec::new();
    let mut final_exit_code = Some(0);

    for (i, step) in steps.iter().enumerate() {
        let step_action = Action {
            id: format!("{}_step_{}", action.id, i),
            name: format!("Step {}", i + 1),
            icon: None,
            action_type: step.step_type.clone(),
            group: None,
            confirm: false,
            enabled: true,
            command: step.command.clone(),
            workdir: step.workdir.clone(),
            env: None,
            path: None,
            args: None,
            url: step.url.clone(),
            method: step.method.clone(),
            headers: step.headers.clone(),
            body: step.body.clone(),
            timeout: None,
            steps: None,
            on_failure: None,
        };

        let result = match step.step_type.as_str() {
            "shell" => execute_shell(&step_action),
            "http" => execute_http(&step_action),
            "url" => execute_url(&step_action),
            _ => Err(format!("Unsupported step type: {}", step.step_type)),
        };

        match result {
            Ok((stdout, stderr, exit_code)) => {
                all_stdout.push(format!("[Step {}] {}", i + 1, stdout));
                if !stderr.is_empty() {
                    all_stderr.push(format!("[Step {}] {}", i + 1, stderr));
                }
                if exit_code.map_or(false, |c| c != 0) {
                    final_exit_code = exit_code;
                    if on_failure == "stop" {
                        all_stderr.push(format!("[Step {}] Failed, stopping pipeline", i + 1));
                        break;
                    }
                }
            }
            Err(err) => {
                all_stderr.push(format!("[Step {}] Error: {}", i + 1, err));
                final_exit_code = Some(1);
                if on_failure == "stop" {
                    break;
                }
            }
        }
    }

    Ok((all_stdout.join("\n"), all_stderr.join("\n"), final_exit_code))
}

fn execute_claude(action: &Action) -> ExecResult {
    let workdir = action.workdir.as_deref()
        .map(|w| resolve_env_vars(w))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    if cfg!(target_os = "windows") {
        let bash_path = find_git_bash()
            .ok_or("Cannot find git-bash. Install Git for Windows or set CLAUDE_CODE_GIT_BASH_PATH")?;

        // Use cmd to set CLAUDE_CODE_GIT_BASH_PATH so claude can find git-bash,
        // then launch claude. /k keeps the window open after claude exits.
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("cmd");
        cmd.raw_arg(format!(
            r#"/C start "" wt -d "{workdir}" cmd /k "set CLAUDE_CODE_GIT_BASH_PATH={bash}&& claude""#,
            workdir = workdir,
            bash = bash_path,
        ));
        run_command(cmd)
    } else if cfg!(target_os = "macos") {
        let script = format!(
            r#"tell application "Terminal"
                activate
                do script "cd '{}' && claude"
            end tell"#,
            workdir
        );
        let mut cmd = Command::new("osascript");
        cmd.args(["-e", &script]);
        run_command(cmd)
    } else {
        // Linux: try common terminal emulators
        let mut cmd = Command::new("bash");
        cmd.args(["-c", &format!(
            r#"cd '{}' && x-terminal-emulator -e bash -lic claude 2>/dev/null || gnome-terminal -- bash -lic claude 2>/dev/null || konsole -e bash -lic claude 2>/dev/null || xterm -e bash -lic claude"#,
            workdir
        )]);
        run_command(cmd)
    }
}

#[cfg(target_os = "windows")]
fn find_git_bash() -> Option<String> {
    use std::path::Path;

    // 1. Check CLAUDE_CODE_GIT_BASH_PATH env var
    if let Ok(path) = std::env::var("CLAUDE_CODE_GIT_BASH_PATH") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    // 2. Check PATH — prefer Git-bundled bash over WSL/store aliases
    if let Ok(output) = Command::new("where").arg("bash").output() {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let path = line.trim();
                // Skip WSL and Windows Store bash
                if path.contains(r"\Windows\") || path.contains("WindowsApps") {
                    continue;
                }
                if Path::new(path).exists() {
                    return Some(path.to_string());
                }
            }
        }
    }

    // 3. Check common locations
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for candidate in &candidates {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }

    None
}

#[cfg(not(target_os = "windows"))]
fn find_git_bash() -> Option<String> {
    Some("bash".to_string())
}

fn run_command(mut cmd: Command) -> ExecResult {
    let output = cmd.output().map_err(|e| format!("Failed to execute command: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code();

    Ok((stdout, stderr, exit_code))
}

fn resolve_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    let re_pattern = regex_lite::Regex::new(r"\$\{(\w+)\}").unwrap();

    for cap in re_pattern.captures_iter(input) {
        let var_name = &cap[1];
        if let Ok(value) = std::env::var(var_name) {
            result = result.replace(&cap[0], &value);
        }
    }

    result
}
