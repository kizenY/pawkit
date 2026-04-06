// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;
use pawkit_lib::cli::Cli;

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Some(cmd) => pawkit_lib::cli::run_cli(cmd),
        None => pawkit_lib::run(),
    }
}
