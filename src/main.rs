use std::{error::Error, process::ExitCode};

use mcp_search::{
    adapters::McpServer,
    application::RuntimeStatus,
    config::{BrowserMode, Config},
};
use rmcp::{ServiceExt, transport::stdio};
use serde::Serialize;

const USAGE: &str = "usage: mcp-search <mcp|status|setup|doctor> [options]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Mcp { profile: Option<String>, lite: bool },
    Status,
    SetupLite,
    Doctor,
}

#[derive(Debug, Serialize)]
struct CliStatus {
    command: &'static str,
    version: &'static str,
    profile: String,
    mode: &'static str,
    browser: &'static str,
    search_backend: &'static str,
    searxng_configured: bool,
    data_dir: String,
    worker_program: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    command: &'static str,
    ok: bool,
    config: &'static str,
    data_dir: String,
    data_dir_exists: bool,
    data_dir_writable: bool,
    notes: Vec<&'static str>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcp-search: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    match parse_command(std::env::args().skip(1))? {
        Command::Mcp { profile, lite } => {
            let mut config = Config::from_env_with_profile(profile.as_deref())?;
            if lite {
                config.browser = BrowserMode::Disabled;
            }
            let service = McpServer::from_config(config)?.serve(stdio()).await?;
            service.waiting().await?;
        }
        Command::Status => {
            let config = Config::from_env()?;
            print_status(&config)?;
        }
        Command::SetupLite => print_setup_lite(Config::from_env()?)?,
        Command::Doctor => print_doctor(Config::from_env()?)?,
    }
    Ok(())
}

fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, &'static str> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("mcp") => {
            let mut profile = None;
            let mut lite = false;
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--lite" if !lite => lite = true,
                    "--profile" if profile.is_none() => {
                        profile = Some(args.next().ok_or("--profile requires NAME")?);
                    }
                    _ => return Err(USAGE),
                }
            }
            Ok(Command::Mcp { profile, lite })
        }
        Some("status") if args.next().is_none() => Ok(Command::Status),
        Some("doctor") if args.next().is_none() => Ok(Command::Doctor),
        Some("setup") => match (args.next().as_deref(), args.next()) {
            (Some("--lite"), None) => Ok(Command::SetupLite),
            (None, None) => {
                Err("unsupported: full setup requires an installer and Chromium package")
            }
            _ => Err(USAGE),
        },
        Some("install" | "update" | "purge") => Err(
            "unsupported: installers, package distribution, and update management are not shipped",
        ),
        _ => Err(USAGE),
    }
}

fn print_status(config: &Config) -> Result<(), Box<dyn Error>> {
    let status = RuntimeStatus::new(&config.profile).map_err(str::to_owned)?;
    let browser = match config.browser {
        BrowserMode::Auto => "auto",
        BrowserMode::Enabled => "enabled",
        BrowserMode::Disabled => "disabled",
    };
    println!(
        "{}",
        serde_json::to_string(&CliStatus {
            command: "status",
            version: status.version,
            profile: status.profile,
            mode: if config.browser == BrowserMode::Disabled {
                "lite"
            } else {
                "source"
            },
            browser,
            search_backend: if config.searxng_url.is_some() {
                "searxng"
            } else {
                "duckduckgo"
            },
            searxng_configured: config.searxng_url.is_some(),
            data_dir: config.data_dir.display().to_string(),
            worker_program: config.worker_program.display().to_string(),
        })?
    );
    Ok(())
}

fn print_setup_lite(config: Config) -> Result<(), Box<dyn Error>> {
    let mut config = config;
    config.browser = BrowserMode::Disabled;
    let status = RuntimeStatus::new(&config.profile).map_err(str::to_owned)?;
    println!(
        "{}",
        serde_json::json!({
            "command": "setup",
            "ok": true,
            "mode": "lite",
            "profile": status.profile,
            "version": status.version,
            "changed": false,
            "message": "lite mode is configuration-only; no installer or Chromium download was attempted"
        })
    );
    Ok(())
}

fn print_doctor(config: Config) -> Result<(), Box<dyn Error>> {
    let data_dir = config.data_dir;
    let exists = data_dir.is_dir();
    let writable = if exists {
        std::fs::metadata(&data_dir).is_ok_and(|metadata| !metadata.permissions().readonly())
    } else {
        data_dir.parent().is_some_and(std::path::Path::is_dir)
    };
    let mut notes = Vec::new();
    if config.searxng_url.is_none() {
        notes.push("web_search uses the built-in backend; set MCP_SEARCH_SEARXNG_URL to use a private SearXNG instance");
    }
    notes.push("Chromium installers and package distribution are not shipped");
    let report = DoctorReport {
        command: "doctor",
        ok: writable,
        config: "valid",
        data_dir: data_dir.display().to_string(),
        data_dir_exists: exists,
        data_dir_writable: writable,
        notes,
    };
    println!("{}", serde_json::to_string(&report)?);
    if report.ok {
        Ok(())
    } else {
        Err("doctor: data directory is not writable or its parent is unavailable".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_command};

    #[test]
    fn parses_supported_commands_and_options() {
        assert_eq!(parse_command(["status".into()]), Ok(Command::Status));
        assert_eq!(parse_command(["doctor".into()]), Ok(Command::Doctor));
        assert_eq!(
            parse_command(["setup".into(), "--lite".into()]),
            Ok(Command::SetupLite)
        );
        assert_eq!(
            parse_command([
                "mcp".into(),
                "--lite".into(),
                "--profile".into(),
                "codex".into()
            ]),
            Ok(Command::Mcp {
                profile: Some("codex".into()),
                lite: true
            })
        );
    }

    #[test]
    fn rejects_unsupported_or_ambiguous_commands() {
        assert!(
            parse_command(["setup".into()])
                .unwrap_err()
                .starts_with("unsupported:")
        );
        assert!(
            parse_command(["install".into(), "codex".into()])
                .unwrap_err()
                .starts_with("unsupported:")
        );
        assert!(parse_command(["mcp".into(), "--profile".into()]).is_err());
        assert!(parse_command(["status".into(), "--lite".into()]).is_err());
    }
}
