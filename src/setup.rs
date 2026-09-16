//! Register this binary as an MCP server through the shared Kurir registry.
//!
//! Websift owns the product command and profile arguments; Kurir owns harness
//! paths, entry shapes, delegated CLIs, JSONC merging, backups, and conflicts.

use std::{
    error::Error,
    fmt, fs,
    io::IsTerminal,
    path::{Path, PathBuf},
};

use kurir::{Harness, RegistrationOptions, Scope, ServerSpec};

/// A client this binary knows how to register itself with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    ClaudeCodeCli,
    ClaudeDesktop,
    Codex,
    Cursor,
    Vscode,
    GeminiCli,
    CopilotCli,
    OpenCode,
    Windsurf,
    Zed,
    GenericJson,
}

impl Client {
    /// Every client in the interactive picker.
    pub const ALL: [Client; 12] = [
        Self::ClaudeCode,
        Self::ClaudeCodeCli,
        Self::ClaudeDesktop,
        Self::Codex,
        Self::Cursor,
        Self::Vscode,
        Self::GeminiCli,
        Self::CopilotCli,
        Self::OpenCode,
        Self::Windsurf,
        Self::Zed,
        Self::GenericJson,
    ];

    /// The value accepted by `--client`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::ClaudeCodeCli => "claude-code-cli",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Vscode => "vscode",
            Self::GeminiCli => "gemini-cli",
            Self::CopilotCli => "copilot-cli",
            Self::OpenCode => "opencode",
            Self::Windsurf => "windsurf",
            Self::Zed => "zed",
            Self::GenericJson => "generic-json",
        }
    }

    /// Parse a `--client` value.
    ///
    /// # Errors
    ///
    /// Returns the unknown identifier when it matches no client.
    pub fn parse(value: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|client| client.id() == value)
            .ok_or_else(|| {
                let known: Vec<_> = Self::ALL.into_iter().map(Self::id).collect();
                format!(
                    "unknown --client '{value}'; known clients: {}",
                    known.join(", ")
                )
            })
    }

    /// One line describing the behavior Kurir will use.
    #[must_use]
    fn summary(self) -> &'static str {
        match self {
            Self::ClaudeCode => "merges ~/.claude.json",
            Self::ClaudeCodeCli => "runs `claude mcp add`",
            Self::ClaudeDesktop => "merges claude_desktop_config.json",
            Self::Codex => "runs `codex mcp add`",
            Self::Cursor => "merges ~/.cursor/mcp.json",
            Self::Vscode => "runs `code --add-mcp`",
            Self::GeminiCli => "runs `gemini mcp add`",
            Self::CopilotCli => "merges ~/.copilot/mcp-config.json",
            Self::OpenCode => "merges ~/.config/opencode/opencode.json",
            Self::Windsurf => "merges ~/.codeium/windsurf/mcp_config.json",
            Self::Zed => "merges ~/.config/zed/settings.json",
            Self::GenericJson => "prints a portable snippet, writes nothing",
        }
    }

    fn harness(self) -> Option<Harness> {
        Some(match self {
            Self::ClaudeCode => Harness::ClaudeCode,
            Self::ClaudeCodeCli => Harness::ClaudeCodeCli,
            Self::ClaudeDesktop => Harness::ClaudeDesktop,
            Self::Codex => Harness::Codex,
            Self::Cursor => Harness::Cursor,
            Self::Vscode => Harness::Vscode,
            Self::GeminiCli => Harness::GeminiCli,
            Self::CopilotCli => Harness::CopilotCli,
            Self::OpenCode => Harness::OpenCode,
            Self::Windsurf => Harness::Windsurf,
            Self::Zed => Harness::Zed,
            Self::GenericJson => return None,
        })
    }
}

impl fmt::Display for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:<16} {}", self.id(), self.summary())
    }
}

/// Everything `websift setup` needs to register one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupOptions {
    pub client: Option<Client>,
    pub profile: Option<String>,
    pub name: String,
    pub command: Option<String>,
    /// Overrides the client's default configuration path.
    pub config: Option<PathBuf>,
    pub dry_run: bool,
    pub print: bool,
    pub force: bool,
}

impl Default for SetupOptions {
    fn default() -> Self {
        Self {
            client: None,
            profile: None,
            name: "websift".to_owned(),
            command: None,
            config: None,
            dry_run: false,
            print: false,
            force: false,
        }
    }
}

/// Register the binary with a named client, or run the interactive picker.
///
/// # Errors
///
/// Returns an error when command resolution, client configuration, delegated
/// registration, or interactive confirmation fails.
pub fn run(options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    match options.client {
        Some(client) => register_one(client, options),
        None => run_interactive(options),
    }
}

fn register_one(client: Client, options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    let profile = normalize_profile(options.profile.as_deref().unwrap_or("default"))?;
    let command = resolve_command(options.command.as_deref())?;
    let spec = ServerSpec::stdio(
        options.name.clone(),
        command,
        vec!["mcp".to_owned(), "--profile".to_owned(), profile],
    );

    let Some(harness) = client.harness() else {
        println!(
            "{}",
            serde_json::to_string_pretty(&kurir::registration::snippet_for(&spec))?
        );
        return Ok(());
    };

    let registration = RegistrationOptions {
        scope: Scope::User,
        config: options.config.clone(),
        cwd: std::env::current_dir()?,
        force: options.force,
        dry_run: options.dry_run,
        print: options.print,
    };
    let result = kurir::register(harness, &spec, &registration)
        .map_err(|error| format!("{}: {error}", client.id()))?;
    let target = result.target.as_ref().map_or_else(
        || "delegated client".to_owned(),
        |path| path.display().to_string(),
    );
    println!("{}: {} ({target})", client.id(), result.action);
    Ok(())
}

fn run_interactive(options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    use inquire::{Confirm, MultiSelect, Text};

    if !std::io::stdin().is_terminal() {
        return Err("setup needs a terminal; pass --client <id> to run non-interactively".into());
    }

    let clients = MultiSelect::new("Which clients?", Client::ALL.to_vec())
        .with_page_size(12)
        .prompt()?;
    if clients.is_empty() {
        return Ok(());
    }
    let default_profile = options.profile.as_deref().unwrap_or("default");
    let profile = normalize_profile(
        &Text::new("Profile:")
            .with_default(default_profile)
            .prompt()?,
    )?;
    let command = resolve_command(options.command.as_deref())?;
    let preview_options = SetupOptions {
        profile: Some(profile.clone()),
        command: Some(command.clone()),
        force: true,
        dry_run: true,
        ..options.clone()
    };

    for client in &clients {
        println!("{client}");
        register_one(*client, &preview_options)?;
    }
    if !Confirm::new("Apply these registrations?")
        .with_default(false)
        .prompt()?
    {
        println!("nothing written");
        return Ok(());
    }

    let apply_options = SetupOptions {
        profile: Some(profile),
        command: Some(command),
        force: true,
        ..options.clone()
    };
    for client in clients {
        register_one(client, &apply_options)?;
    }
    Ok(())
}

fn normalize_profile(profile: &str) -> Result<String, String> {
    let profile = profile.trim();
    let valid = !profile.is_empty()
        && profile.len() <= 64
        && profile.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if valid {
        Ok(profile.to_owned())
    } else {
        Err(format!(
            "invalid profile '{profile}': 1-64 characters of ASCII letters, digits, '-', or '_'"
        ))
    }
}

/// Resolve the command to an absolute path so GUI clients do not depend on PATH.
fn resolve_command(command: Option<&str>) -> Result<String, String> {
    let candidate = match command {
        Some(command) => PathBuf::from(command),
        None => std::env::current_exe()
            .map_err(|error| format!("could not determine this executable's path: {error}"))?,
    };
    let resolved = if candidate.components().count() > 1 {
        candidate
    } else {
        which(&candidate).ok_or_else(|| format!("'{}' is not on PATH", candidate.display()))?
    };
    fs::canonicalize(&resolved)
        .map(|path| path.display().to_string())
        .map_err(|error| format!("'{}': {error}", resolved.display()))
}

#[cfg(not(windows))]
fn which(name: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn which(name: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("where")
        .arg(name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("websift-setup-{label}-{nonce}.json"))
    }

    fn options(path: PathBuf) -> SetupOptions {
        SetupOptions {
            client: Some(Client::ClaudeCode),
            command: Some(
                std::env::current_exe()
                    .expect("test binary")
                    .display()
                    .to_string(),
            ),
            config: Some(path),
            ..SetupOptions::default()
        }
    }

    #[test]
    fn kurir_registration_preserves_other_servers() {
        let path = temp_path("preserve");
        fs::write(
            &path,
            r#"{"theme":"dark","mcpServers":{"other":{"command":"other"}}}"#,
        )
        .expect("seed");
        run(&options(path.clone())).expect("register");
        let output: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("output")).expect("json");
        assert_eq!(output["theme"], "dark");
        assert_eq!(output["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            output["mcpServers"]["websift"]["args"],
            serde_json::json!(["mcp", "--profile", "default"])
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let path = temp_path("dry");
        let options = SetupOptions {
            dry_run: true,
            ..options(path.clone())
        };
        run(&options).expect("dry run");
        assert!(!path.exists());
    }

    #[test]
    fn conflicts_require_force() {
        let path = temp_path("conflict");
        fs::write(&path, r#"{"mcpServers":{"websift":{"command":"old"}}}"#).expect("seed");
        let error = run(&options(path.clone())).expect_err("conflict");
        assert!(error.to_string().contains("conflict"), "{error}");
        let forced = SetupOptions {
            force: true,
            ..options(path.clone())
        };
        run(&forced).expect("forced registration");
        let output: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("output")).expect("json");
        assert_eq!(
            output["mcpServers"]["websift"]["args"],
            serde_json::json!(["mcp", "--profile", "default"])
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn every_exposed_client_maps_to_kurir_or_snippet() {
        for client in Client::ALL {
            assert_eq!(Client::parse(client.id()), Ok(client));
        }
        assert!(Client::GenericJson.harness().is_none());
        assert_eq!(Client::OpenCode.harness(), Some(Harness::OpenCode));
    }

    #[test]
    fn profile_rejects_path_escape() {
        assert_eq!(normalize_profile(" work "), Ok("work".to_owned()));
        assert!(normalize_profile("../etc").is_err());
        assert!(normalize_profile("").is_err());
        assert!(normalize_profile(&"a".repeat(65)).is_err());
    }
}
