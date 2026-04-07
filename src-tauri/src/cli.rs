use crate::config::{get_config_dir, load_actions};
use crate::executor::execute_action;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "pawkit", about = "Desktop pet with customizable quick actions")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run an action by ID
    Run {
        /// Action ID (kebab-case)
        action_id: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
    /// List all available actions
    List {
        /// Filter by group name
        #[arg(short, long)]
        group: Option<String>,
    },
}

pub fn run_cli(cmd: &Commands) {
    let config_dir = get_config_dir();
    let actions_config = load_actions(&config_dir);

    match cmd {
        Commands::List { group } => {
            let actions: Vec<_> = actions_config
                .actions
                .iter()
                .filter(|a| a.enabled)
                .filter(|a| {
                    group
                        .as_ref()
                        .map_or(true, |g| a.group.as_deref() == Some(g.as_str()))
                })
                .collect();

            if actions.is_empty() {
                println!("No actions found.");
                return;
            }

            // Group actions using BTreeMap for consistent sorted output
            let mut groups: BTreeMap<Option<&str>, Vec<_>> = BTreeMap::new();
            for action in &actions {
                groups.entry(action.group.as_deref()).or_default().push(action);
            }

            let mut first = true;
            for (group_name, group_actions) in &groups {
                if !first {
                    println!();
                }
                first = false;
                if let Some(name) = group_name {
                    println!("[{}]", name);
                }
                for action in group_actions {
                    let icon = action.icon.as_deref().unwrap_or(" ");
                    let confirm_mark = if action.confirm { " ⚠" } else { "" };
                    println!(
                        "  {} {:<24} {}{}",
                        icon, action.id, action.name, confirm_mark
                    );
                }
            }
        }
        Commands::Run { action_id, yes } => {
            let action = actions_config
                .actions
                .iter()
                .find(|a| a.id == *action_id && a.enabled);

            let action = match action {
                Some(a) => a,
                None => {
                    eprintln!("Action '{}' not found. Use `pawkit list` to see available actions.", action_id);
                    std::process::exit(1);
                }
            };

            if action.confirm && !yes {
                let is_tty = atty::is(atty::Stream::Stdin);
                if !is_tty {
                    eprintln!("Action '{}' requires confirmation. Use -y to skip, or run from an interactive terminal.", action.name);
                    std::process::exit(1);
                }
                print!("Run '{}'? [y/N] ", action.name);
                let _ = io::stdout().flush();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).unwrap_or(0) == 0
                    || !input.trim().eq_ignore_ascii_case("y")
                {
                    println!("Cancelled.");
                    return;
                }
            }

            let result = execute_action(action);

            if !result.stdout.is_empty() {
                print!("{}", result.stdout);
            }
            if !result.stderr.is_empty() {
                eprint!("{}", result.stderr);
            }

            std::process::exit(result.exit_code.unwrap_or(if result.success { 0 } else { 1 }));
        }
    }
}
