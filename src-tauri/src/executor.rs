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
        "shell" => execute_shell(action),
        "script" => execute_script(action),
        "url" => execute_url(action),
        "http" => execute_http(action),
        "pipeline" => execute_pipeline(action),
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

    let mut cmd = Command::new("cmd");
    cmd.args(["/C", command]);

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
    } else {
        ("cmd", vec!["/C", path])
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
