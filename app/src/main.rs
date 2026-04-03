#![allow(unused)]

// prevents console by default
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sprites;
mod parser;
mod sensing;
mod base;
mod worker;

use base::{log_user, MYTHIC_VERSION};

fn main() -> std::process::ExitCode {
    let Some(_lock_file) = worker::init() else { return std::process::ExitCode::FAILURE };

    let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
    log_user!("Starting Mythic Monitor v{}.{} log={} at={ts:>011}", MYTHIC_VERSION.major, MYTHIC_VERSION.minor, base::logger::levelf_as_str(log::max_level()));

    while worker::run() {
        let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
        log_user!("Restarting Mythic Monitor at={ts:>011}");
    }

    let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
    log_user!("Exiting Mythic Monitor at={ts:>011}");

    std::process::ExitCode::SUCCESS
}