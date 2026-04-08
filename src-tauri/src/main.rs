// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;
use pawkit_lib::cli::Cli;

/// In release builds on Windows, `windows_subsystem = "windows"` hides the
/// console. CLI subcommands need stdout/stderr, so we re-attach to the parent
/// console (the terminal the user launched us from).
#[cfg(target_os = "windows")]
fn attach_parent_console() {
    unsafe {
        extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
            fn AllocConsole() -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            // No parent console (e.g. double-clicked .exe) — allocate one
            AllocConsole();
        }
    }
}

fn main() {
    #[cfg(target_os = "windows")]
    if std::env::args().len() > 1 {
        attach_parent_console();
    }

    let cli = Cli::parse();

    match &cli.command {
        Some(cmd) => pawkit_lib::cli::run_cli(cmd),
        None => pawkit_lib::run(),
    }
}
