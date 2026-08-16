use std::{error::Error, process::ExitCode, time::Duration};

use rmcp::{ServiceExt, transport::stdio};
use serde::Serialize;
use websift::{
    adapters::McpServer,
    application::RuntimeStatus,
    config::{BrowserMode, Config},
    setup::{Client, SetupOptions},
    update::{Updater, replace_executable},
};

const USAGE: &str = "usage: websift <mcp|status|setup|doctor|update> [options]";
const SETUP_USAGE: &str = "usage: websift setup [--client ID] [--profile NAME] [--name NAME] \
[--command PATH] [--config PATH] [--dry-run] [--print] [--force]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Mcp { profile: Option<String>, lite: bool },
    Status,
    Setup(Box<SetupOptions>),
    SetupLite,
    Doctor,
    Update { check_only: bool },
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
            eprintln!("websift: {error}");
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
        Command::Setup(options) => websift::setup::run(&options)?,
        Command::SetupLite => print_setup_lite(Config::from_env()?)?,
        Command::Doctor => print_doctor(Config::from_env()?)?,
        Command::Update { check_only } => run_update(check_only).await?,
    }
    Ok(())
}

async fn run_update(check_only: bool) -> Result<(), Box<dyn Error>> {
    let updater = Updater::new(Duration::from_secs(30))?;
    let status = updater.check().await?;

    if !status.available {
        println!(
            "{}",
            serde_json::json!({
                "command": "update",
                "current": status.current,
                "latest": status.latest_version(),
                "latest_tag": status.latest_tag,
                "update_available": false,
                "changed": false,
                "message": "already on the latest release",
            })
        );
        return Ok(());
    }

    if check_only {
        println!(
            "{}",
            serde_json::json!({
                "command": "update",
                "current": status.current,
                "latest": status.latest_version(),
                "latest_tag": status.latest_tag,
                "update_available": true,
                "changed": false,
                "message": "run `websift update` to install it",
            })
        );
        return Ok(());
    }

    // Resolve the real path first: replacing a symlink would leave the installed binary untouched.
    let executable = std::env::current_exe()?.canonicalize()?;
    let binary = updater.download_verified(&status.latest_tag).await?;
    replace_executable(&executable, &binary)?;
    println!(
        "{}",
        serde_json::json!({
            "command": "update",
            "current": status.current,
            "latest": status.latest_version(),
                "latest_tag": status.latest_tag,
            "update_available": true,
            "changed": true,
            "path": executable.display().to_string(),
            "message": "updated; restart any running websift MCP server",
        })
    );
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
        Some("setup") => parse_setup(args),
        Some("update") => match (args.next().as_deref(), args.next()) {
            (None, None) => Ok(Command::Update { check_only: false }),
            (Some("--check"), None) => Ok(Command::Update { check_only: true }),
            _ => Err(USAGE),
        },
        Some("install" | "purge") => {
            Err("unsupported: installers and package removal are not shipped")
        }
        _ => Err(USAGE),
    }
}

/// `setup --lite` keeps its configuration-only JSON contract; every other form builds a
/// [`SetupOptions`] for the client installer.
fn parse_setup(args: impl Iterator<Item = String>) -> Result<Command, &'static str> {
    let mut args = args.peekable();
    if args.peek().map(String::as_str) == Some("--lite") {
        args.next();
        return if args.next().is_none() {
            Ok(Command::SetupLite)
        } else {
            Err(SETUP_USAGE)
        };
    }

    let mut options = SetupOptions::default();
    while let Some(arg) = args.next() {
        let mut value = |flag: &'static str| args.next().ok_or(flag);
        match arg.as_str() {
            "--client" if options.client.is_none() => {
                let id = value("--client requires ID")?;
                options.client = Some(Client::parse(&id).map_err(|_| "unknown --client ID")?);
            }
            "--profile" if options.profile.is_none() => {
                options.profile = Some(value("--profile requires NAME")?);
            }
            "--name" => options.name = value("--name requires NAME")?,
            "--command" if options.command.is_none() => {
                options.command = Some(value("--command requires PATH")?);
            }
            "--config" if options.config.is_none() => {
                options.config = Some(value("--config requires PATH")?.into());
            }
            "--dry-run" if !options.dry_run => options.dry_run = true,
            "--print" if !options.print => options.print = true,
            "--force" if !options.force => options.force = true,
            _ => return Err(SETUP_USAGE),
        }
    }

    Ok(Command::Setup(Box::new(options)))
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
        notes.push("web_search uses the built-in backend; set WEBSIFT_SEARXNG_URL to use a private SearXNG instance");
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
    use websift::setup::Client;

    fn setup(args: &[&str]) -> Result<websift::setup::SetupOptions, &'static str> {
        let args = std::iter::once("setup".to_owned()).chain(args.iter().map(|a| (*a).to_owned()));
        match parse_command(args)? {
            Command::Setup(options) => Ok(*options),
            other => panic!("expected Setup, got {other:?}"),
        }
    }

    #[test]
    fn setup_without_arguments_selects_the_interactive_installer() {
        let options = setup(&[]).expect("parse");
        assert_eq!(options.client, None);
        assert_eq!(options.name, "websift");
    }

    #[test]
    fn setup_lite_still_parses_to_the_configuration_only_command() {
        assert_eq!(
            parse_command(["setup".into(), "--lite".into()]),
            Ok(Command::SetupLite)
        );
        assert!(parse_command(["setup".into(), "--lite".into(), "--force".into()]).is_err());
    }

    #[test]
    fn setup_accepts_every_installer_flag() {
        let options = setup(&[
            "--client",
            "codex",
            "--profile",
            "work",
            "--name",
            "ws",
            "--command",
            "/bin/ws",
            "--config",
            "/tmp/c.json",
            "--dry-run",
            "--print",
            "--force",
        ])
        .expect("parse");
        assert_eq!(options.client, Some(Client::Codex));
        assert_eq!(options.profile.as_deref(), Some("work"));
        assert_eq!(options.name, "ws");
        assert_eq!(options.command.as_deref(), Some("/bin/ws"));
        assert_eq!(options.config, Some("/tmp/c.json".into()));
        assert!(options.dry_run && options.print && options.force);
    }

    #[test]
    fn setup_rejects_an_unknown_client_and_a_flag_missing_its_value() {
        assert!(setup(&["--client", "emacs"]).is_err());
        assert!(setup(&["--client"]).is_err());
        assert!(setup(&["--profile"]).is_err());
        assert!(setup(&["--nope"]).is_err());
    }

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
        assert_eq!(
            parse_command(["update".into()]),
            Ok(Command::Update { check_only: false })
        );
        assert_eq!(
            parse_command(["update".into(), "--check".into()]),
            Ok(Command::Update { check_only: true })
        );
    }

    #[test]
    fn rejects_unsupported_or_ambiguous_commands() {
        assert!(
            parse_command(["install".into(), "codex".into()])
                .unwrap_err()
                .starts_with("unsupported:")
        );
        assert!(parse_command(["mcp".into(), "--profile".into()]).is_err());
        assert!(parse_command(["status".into(), "--lite".into()]).is_err());
        // An unknown update flag must not silently fall through to a mutating update.
        assert!(parse_command(["update".into(), "--force".into()]).is_err());
        assert!(parse_command(["update".into(), "--check".into(), "extra".into()]).is_err());
    }
}
