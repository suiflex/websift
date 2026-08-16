//! Registration of this binary as an MCP server in a client's configuration.
//!
//! Two shapes of client exist. A [`ClientDescriptor::FileTarget`] keeps its servers in a JSON
//! file that this module merges into; a [`ClientDescriptor::Delegated`] ships its own CLI, which
//! is invoked rather than second-guessed. [`ClientDescriptor::SnippetOnly`] prints a portable
//! entry for anything not modelled here.
//!
//! Writing outside the profile data directory is the one thing this crate does to files it does
//! not own, so every write is preceded by a preview, a confirmation (interactively) or an
//! explicit flag, and a `.bak` copy. A configuration that cannot be parsed is reported, never
//! replaced.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt, fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::Command as Process,
};

use serde_json::{Map, Value, json};

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
    /// Every client, in the order the interactive picker offers them.
    pub const ALL: [Client; 12] = [
        Client::ClaudeCode,
        Client::ClaudeCodeCli,
        Client::ClaudeDesktop,
        Client::Codex,
        Client::Cursor,
        Client::Vscode,
        Client::GeminiCli,
        Client::CopilotCli,
        Client::OpenCode,
        Client::Windsurf,
        Client::Zed,
        Client::GenericJson,
    ];

    /// The value accepted by `--client`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::ClaudeCodeCli => "claude-code-cli",
            Client::ClaudeDesktop => "claude-desktop",
            Client::Codex => "codex",
            Client::Cursor => "cursor",
            Client::Vscode => "vscode",
            Client::GeminiCli => "gemini-cli",
            Client::CopilotCli => "copilot-cli",
            Client::OpenCode => "opencode",
            Client::Windsurf => "windsurf",
            Client::Zed => "zed",
            Client::GenericJson => "generic-json",
        }
    }

    /// Parse a `--client` value.
    ///
    /// # Errors
    ///
    /// Returns the unknown identifier when it matches no client.
    pub fn parse(value: &str) -> Result<Self, String> {
        Client::ALL
            .into_iter()
            .find(|client| client.id() == value)
            .ok_or_else(|| {
                let known: Vec<&str> = Client::ALL.into_iter().map(Client::id).collect();
                format!(
                    "unknown --client '{value}'; known clients: {}",
                    known.join(", ")
                )
            })
    }

    /// One line describing where the entry lands, shown in the picker.
    #[must_use]
    fn summary(self) -> &'static str {
        match self {
            Client::ClaudeCode => "writes ~/.claude.json",
            Client::ClaudeCodeCli => "runs `claude mcp add`",
            Client::ClaudeDesktop => "writes claude_desktop_config.json",
            Client::Codex => "runs `codex mcp add`",
            Client::Cursor => "writes ~/.cursor/mcp.json",
            Client::Vscode => "runs `code --add-mcp`",
            Client::GeminiCli => "runs `gemini mcp add`",
            Client::CopilotCli => "writes ~/.copilot/mcp-config.json",
            Client::OpenCode => "writes ~/.config/opencode/opencode.jsonc",
            Client::Windsurf => "writes ~/.codeium/windsurf/mcp_config.json (provisional)",
            Client::Zed => "writes ~/.config/zed/settings.json (provisional)",
            Client::GenericJson => "prints a portable snippet, writes nothing",
        }
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
    /// Overrides the client's default configuration path. Also the seam the tests write through.
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

/// How a client stores its MCP servers.
enum ClientDescriptor {
    /// A JSON (or JSONC) file this module merges into.
    FileTarget {
        path: PathBuf,
        /// Top-level key holding the server map.
        key: &'static str,
    },
    /// A client CLI that owns its own configuration format.
    Delegated { program: &'static str },
    /// No configuration is written; the entry is printed for the user to place.
    SnippetOnly,
}

/// The entry as it appears inside the server map, and as a standalone snippet.
struct ServerSpec {
    entry: Value,
    snippet: Value,
}

/// Register the binary with one client, or run the interactive picker when none was named.
///
/// # Errors
///
/// Returns an error when the client's configuration cannot be read or written, when an entry
/// already exists and `--force` was not given, when a delegated client CLI fails, or when the
/// picker is needed but stdin is not a terminal./// Terminal branding: ANSI colors, banner, and bordered panels.
///
/// Stdlib only — no `owo-colors`, no `console`. Colors are 256-color approximations of the
/// suitest palette so the two CLIs read as one product, and everything collapses to plain text
/// when stdout is not a terminal or `NO_COLOR` is set.
mod theme {
    use std::io::IsTerminal;

    use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};

    pub(super) const ACCENT: &str = "\x1b[38;5;114m"; // #4ade80
    pub(super) const RED: &str = "\x1b[38;5;210m"; // #f87171
    pub(super) const AMBER: &str = "\x1b[38;5;221m"; // #fbbf24
    pub(super) const VIOLET: &str = "\x1b[38;5;146m"; // #a78bfa
    const BOLD_FG: &str = "\x1b[1;38;5;255m"; // #fafafa
    const RESET: &str = "\x1b[0m";

    fn enabled() -> bool {
        std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
    }

    fn paint(color: &str, text: &str) -> String {
        if enabled() {
            format!("{color}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    /// Boxed wordmark. The icon is an outlined mini-box holding a funnel — websift sifts the web
    /// down to what was asked for. Box-drawing characters, not filled blocks, which render as a
    /// solid blob at small sizes.
    pub(super) fn banner() -> String {
        let rows = ["┌───┐", "│ ▽ │  W E B S I F T", "└───┘"];
        let width = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0)
            + 4;
        let mut out = vec![format!("┌{}┐", "─".repeat(width))];
        for row in rows {
            let pad = width - 4 - row.chars().count();
            out.push(format!("│  {row}{}  │", " ".repeat(pad)));
        }
        out.push(format!("└{}┘", "─".repeat(width)));
        paint(ACCENT, &out.join("\n"))
    }

    /// A row that sits on the connector column, marker at column zero.
    pub(super) fn point(text: &str, color: &str) -> String {
        paint(color, &format!("◇ {text}"))
    }

    /// One step of the flow: a labeled rule, then body lines under a shared gutter.
    pub(super) fn step(label: &str, lines: &[String], color: &str) -> String {
        let rule = "─".repeat(30usize.saturating_sub(label.len()).max(3));
        let mut out = vec![point(&format!("{label} {rule}"), color), gutter("", color)];
        for line in lines {
            out.push(gutter(&paint(BOLD_FG, line), color));
        }
        out.push(gutter("", color));
        out.join("\n")
    }

    fn gutter(text: &str, color: &str) -> String {
        let bar = paint(color, "│");
        if text.is_empty() {
            bar
        } else {
            format!("{bar} {text}")
        }
    }

    /// Bordered panel around a block of lines.
    pub(super) fn panel(lines: &[String], title: &str, color: &str) -> String {
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .chain(std::iter::once(title.chars().count()))
            .max()
            .unwrap_or(0)
            .max(20);
        let mut out = vec![paint(
            color,
            &format!(
                "┌─ {title} {}┐",
                "─".repeat((width + 2).saturating_sub(title.chars().count() + 3))
            ),
        )];
        for line in lines {
            let pad = " ".repeat(width - line.chars().count());
            out.push(format!(
                "{} {}{pad} {}",
                paint(color, "│"),
                paint(BOLD_FG, line),
                paint(color, "│")
            ));
        }
        out.push(paint(color, &format!("└{}┘", "─".repeat(width + 2))));
        out.join("\n")
    }

    /// Bind the prompt widgets to the same accent the rest of the flow uses.
    pub(super) fn render_config() -> RenderConfig<'static> {
        if !enabled() {
            return RenderConfig::empty();
        }
        let accent = Color::LightGreen;
        RenderConfig::default()
            .with_prompt_prefix(Styled::new("◇").with_fg(accent))
            .with_answered_prompt_prefix(Styled::new("◇").with_fg(accent))
            .with_highlighted_option_prefix(Styled::new("›").with_fg(accent))
            .with_selected_checkbox(Styled::new("◼").with_fg(accent))
            .with_unselected_checkbox(Styled::new("◻").with_fg(Color::DarkGrey))
            .with_answer(StyleSheet::new().with_fg(accent))
            .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
            .with_option(StyleSheet::empty())
            .with_selected_option(Some(
                StyleSheet::new()
                    .with_fg(accent)
                    .with_attr(Attributes::BOLD),
            ))
    }
}

/// What one client installation will do, decided before anything is written so the preview and
/// the write share a single source of truth.
enum Plan {
    File {
        path: PathBuf,
        key: &'static str,
        entry: Value,
        existing: bool,
    },
    Delegated {
        steps: Vec<Vec<String>>,
    },
    Snippet,
}

/// A resolved installation: the client, its plan, and the snippet describing the entry.
struct Installation {
    client: Client,
    plan: Plan,
    snippet: Value,
}

impl Installation {
    /// The lines shown in the preview, and by `--dry-run`.
    fn lines(&self) -> Vec<String> {
        match &self.plan {
            Plan::File {
                path,
                key,
                existing,
                ..
            } => {
                let verb = if *existing { "replace" } else { "add" };
                vec![
                    format!("{verb} '{key}' entry"),
                    format!("in {}", path.display()),
                ]
            }
            Plan::Delegated { steps } => steps
                .iter()
                .map(|step| format!("$ {}", shell_join(step)))
                .collect(),
            Plan::Snippet => vec!["print a snippet; no file is written".to_owned()],
        }
    }
}

/// Register the binary with the named clients, or run the interactive picker when none was named.
///
/// # Errors
///
/// Returns an error when a client's configuration cannot be read or written, when an entry
/// already exists and `--force` was not given, when a delegated client CLI fails, or when the
/// picker is needed but stdin is not a terminal.
pub fn run(options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    match options.client {
        Some(client) => {
            let profile = options
                .profile
                .clone()
                .unwrap_or_else(|| "default".to_owned());
            let installation = resolve(client, &profile, options)?;
            if options.print {
                println!("{}", serde_json::to_string_pretty(&installation.snippet)?);
            }
            for line in installation.lines() {
                println!("{}: {line}", client.id());
            }
            if options.dry_run {
                return Ok(());
            }
            apply(&installation, options)
        }
        None => run_interactive(options),
    }
}

fn run_interactive(options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    use inquire::{Confirm, MultiSelect, Text};

    if !std::io::stdin().is_terminal() {
        return Err("setup needs a terminal; pass --client <id> to run non-interactively".into());
    }

    println!("{}\n", theme::banner());

    match resolve_command(options.command.as_deref()) {
        Ok(command) => {
            println!(
                "{}\n",
                theme::step(
                    "setup — preflight",
                    &[format!("[ok]   binary: {command}")],
                    theme::ACCENT,
                )
            );
        }
        Err(error) => {
            println!(
                "{}\n",
                theme::step(
                    "setup — preflight",
                    &[format!("[fail] {error}")],
                    theme::RED
                )
            );
            return Err(error.into());
        }
    }

    let clients = MultiSelect::new("Which clients?", Client::ALL.to_vec())
        .with_page_size(12)
        .with_help_message("↑↓ move · space toggle · → all · ← none · enter confirm")
        .with_render_config(theme::render_config())
        .prompt()?;
    if clients.is_empty() {
        println!("{}", theme::point("nothing selected", theme::AMBER));
        return Ok(());
    }

    let default_profile = options
        .profile
        .clone()
        .unwrap_or_else(|| "default".to_owned());
    let profile = Text::new("Profile:")
        .with_default(&default_profile)
        .with_help_message("isolation namespace: its own database, crawl jobs, and page cache")
        .with_render_config(theme::render_config())
        .prompt()?;
    let profile = normalize_profile(&profile)?;

    // Confirming a preview that says "replace" is the consent `--force` asks for on the flag path.
    let confirmed = SetupOptions {
        force: true,
        ..options.clone()
    };

    let mut installations = Vec::with_capacity(clients.len());
    for client in clients {
        installations.push(resolve(client, &profile, &confirmed)?);
    }

    let mut body = Vec::new();
    for installation in &installations {
        body.push(format!("{}:", installation.client.id()));
        for line in installation.lines() {
            body.push(format!("  {line}"));
        }
    }
    println!(
        "\n{}\n",
        theme::panel(&body, "planned changes", theme::VIOLET)
    );

    if !Confirm::new("Apply this?")
        .with_default(false)
        .with_render_config(theme::render_config())
        .prompt()?
    {
        println!("{}", theme::point("nothing written", theme::AMBER));
        return Ok(());
    }

    println!();
    for installation in &installations {
        apply(installation, &confirmed)?;
    }
    println!(
        "\n{}",
        theme::point(
            "done — restart the client to pick up the new server",
            theme::ACCENT
        )
    );
    Ok(())
}

fn normalize_profile(profile: &str) -> Result<String, String> {
    let profile = profile.trim();
    if profile.is_empty() {
        return Err("profile must not be empty".to_owned());
    }
    // Mirrors the bound in `RuntimeStatus::new`: the value becomes a filename and a storage key.
    let valid = profile.len() <= 64
        && profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(profile.to_owned())
    } else {
        Err(format!(
            "invalid profile '{profile}': 1-64 characters of ASCII letters, digits, '-', or '_'"
        ))
    }
}

/// Decide what will happen without touching anything.
fn resolve(
    client: Client,
    profile: &str,
    options: &SetupOptions,
) -> Result<Installation, Box<dyn Error>> {
    let command = resolve_command(options.command.as_deref())?;
    let spec = server_spec(client, &options.name, &command, profile);

    let plan = match describe(client, options.config.as_deref())? {
        ClientDescriptor::SnippetOnly => Plan::Snippet,
        ClientDescriptor::Delegated { program } => Plan::Delegated {
            steps: delegated_steps(
                client,
                program,
                &options.name,
                &command,
                profile,
                options.force,
            ),
        },
        ClientDescriptor::FileTarget { path, key } => {
            // Reading now surfaces an unparsable or wrongly-shaped configuration in the preview,
            // before the user is asked to approve a write that would fail anyway.
            let mut root = load_json_object(&path)?;
            let servers = ensure_object_field(&mut root, key)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let existing = servers.contains_key(&options.name);
            if existing && !options.force {
                return Err(format!(
                    "{}: '{}' already exists in {key}; pass --force to replace it",
                    path.display(),
                    options.name
                )
                .into());
            }
            Plan::File {
                path,
                key,
                entry: spec.entry,
                existing,
            }
        }
    };

    Ok(Installation {
        client,
        plan,
        snippet: spec.snippet,
    })
}

fn apply(installation: &Installation, options: &SetupOptions) -> Result<(), Box<dyn Error>> {
    match &installation.plan {
        Plan::Snippet => {
            println!("{}", serde_json::to_string_pretty(&installation.snippet)?);
            Ok(())
        }
        Plan::Delegated { steps } => {
            for step in steps {
                run_step(step)?;
            }
            println!(
                "{}",
                theme::point(
                    &format!(
                        "{}: registered '{}'",
                        installation.client.id(),
                        options.name
                    ),
                    theme::ACCENT
                )
            );
            Ok(())
        }
        Plan::File {
            path, key, entry, ..
        } => {
            // Re-read rather than carrying the parsed document from `resolve`: the preview and the
            // confirmation are separated by user input, and the file may have changed underneath.
            let mut root = load_json_object(path)?;
            let servers = ensure_object_field(&mut root, key)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            servers.insert(options.name.clone(), entry.clone());
            backup_if_exists(path)?;
            write_json_object(path, &root)
        }
    }
}

fn server_spec(client: Client, name: &str, command: &str, profile: &str) -> ServerSpec {
    let args = json!(["mcp", "--profile", profile]);
    let (key, entry) = match client {
        Client::OpenCode => (
            "mcp",
            json!({
                "type": "local",
                "command": [command, "mcp", "--profile", profile],
                "enabled": true,
            }),
        ),
        Client::Zed => (
            "context_servers",
            json!({ "source": "custom", "command": command, "args": args }),
        ),
        Client::CopilotCli => (
            "mcpServers",
            json!({ "type": "stdio", "command": command, "args": args }),
        ),
        _ => ("mcpServers", json!({ "command": command, "args": args })),
    };

    ServerSpec {
        snippet: json!({ key: { name: entry.clone() } }),
        entry,
    }
}

fn describe(
    client: Client,
    config_override: Option<&Path>,
) -> Result<ClientDescriptor, Box<dyn Error>> {
    let delegated = |program| Ok(ClientDescriptor::Delegated { program });
    let file = |default: PathBuf, key| {
        Ok(ClientDescriptor::FileTarget {
            path: config_override.map_or(default, Path::to_path_buf),
            key,
        })
    };

    match client {
        Client::ClaudeCodeCli => return delegated("claude"),
        Client::Codex => return delegated("codex"),
        Client::Vscode => return delegated("code"),
        Client::GeminiCli => return delegated("gemini"),
        Client::GenericJson => return Ok(ClientDescriptor::SnippetOnly),
        _ => {}
    }

    let home = home_dir().ok_or("could not determine the home directory")?;
    match client {
        Client::ClaudeCode => file(home.join(".claude.json"), "mcpServers"),
        Client::ClaudeDesktop => file(claude_desktop_config(&home), "mcpServers"),
        Client::Cursor => file(home.join(".cursor").join("mcp.json"), "mcpServers"),
        Client::CopilotCli => file(home.join(".copilot").join("mcp-config.json"), "mcpServers"),
        Client::OpenCode => file(
            home.join(".config").join("opencode").join("opencode.jsonc"),
            "mcp",
        ),
        Client::Windsurf => file(
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            "mcpServers",
        ),
        Client::Zed => file(
            home.join(".config").join("zed").join("settings.json"),
            "context_servers",
        ),
        Client::ClaudeCodeCli
        | Client::Codex
        | Client::Vscode
        | Client::GeminiCli
        | Client::GenericJson => unreachable!("handled above"),
    }
}

fn delegated_steps(
    client: Client,
    program: &'static str,
    name: &str,
    command: &str,
    profile: &str,
    force: bool,
) -> Vec<Vec<String>> {
    let own = |parts: &[&str]| {
        parts
            .iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>()
    };
    let mut steps = Vec::new();

    match client {
        Client::Vscode => {
            // `code --add-mcp` takes the whole entry as one JSON argument and upserts it.
            let entry = json!({
                "name": name,
                "command": command,
                "args": ["mcp", "--profile", profile],
            });
            steps.push(own(&[program, "--add-mcp", &entry.to_string()]));
        }
        Client::ClaudeCodeCli => {
            if force {
                steps.push(own(&[program, "mcp", "remove", "--scope", "user", name]));
            }
            steps.push(own(&[
                program,
                "mcp",
                "add",
                "--scope",
                "user",
                name,
                "--",
                command,
                "mcp",
                "--profile",
                profile,
            ]));
        }
        Client::GeminiCli => {
            if force {
                steps.push(own(&[program, "mcp", "remove", name]));
            }
            steps.push(own(&[
                program,
                "mcp",
                "add",
                "-s",
                "user",
                name,
                command,
                "mcp",
                "--profile",
                profile,
            ]));
        }
        _ => {
            if force {
                steps.push(own(&[program, "mcp", "remove", name]));
            }
            steps.push(own(&[
                program,
                "mcp",
                "add",
                name,
                "--",
                command,
                "mcp",
                "--profile",
                profile,
            ]));
        }
    }

    steps
}

fn run_step(step: &[String]) -> Result<(), Box<dyn Error>> {
    let (program, args) = step.split_first().ok_or("empty command")?;
    let status = Process::new(program).args(args).status().map_err(|error| {
        format!("could not run '{program}': {error}; is it installed and on PATH?")
    })?;
    if status.success() {
        Ok(())
    } else {
        // A removal before an add is best-effort only when forcing; every other step is fatal.
        Err(format!("`{}` failed with {status}", shell_join(step)).into())
    }
}

/// Resolve the command a client should launch to an absolute path.
///
/// Desktop clients do not inherit the shell `PATH`, so a bare name is not enough.
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

    let resolved = fs::canonicalize(&resolved)
        .map_err(|error| format!("'{}': {error}", resolved.display()))?;
    Ok(resolved.display().to_string())
}

fn which(name: &Path) -> Option<PathBuf> {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let extensions: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let path = std::env::var_os("PATH")?;
    for directory in path.to_string_lossy().split(separator) {
        for extension in extensions {
            let mut candidate = Path::new(directory).join(name);
            if !extension.is_empty() {
                candidate.set_extension(&extension[1..]);
            }
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

fn claude_desktop_config(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map_or_else(|| home.join("AppData/Roaming"), PathBuf::from)
            .join("Claude")
            .join("claude_desktop_config.json")
    } else {
        home.join(".config/Claude/claude_desktop_config.json")
    }
}

/// Read a client configuration, tolerating JSONC comments. A missing file is an empty object.
fn load_json_object(path: &Path) -> Result<Map<String, Value>, Box<dyn Error>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("{}: {error}", path.display()).into()),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&strip_comments(&text))
        .map_err(|error| format!("{}: not valid JSON ({error})", path.display()))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(format!(
            "{}: expected a JSON object at the top level",
            path.display()
        )
        .into()),
    }
}

/// Remove `//` and `/* */` comments, leaving string literals untouched.
fn strip_comments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            output.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                output.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        output.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            _ => output.push(c),
        }
    }

    output
}

/// Borrow the object stored at `key`, creating it when absent.
///
/// A non-object already at that key is an error: the user put something there deliberately, and
/// replacing it would discard their configuration.
fn ensure_object_field<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, String> {
    let slot = root.entry(key.to_owned()).or_insert_with(|| json!({}));
    match slot {
        Value::Object(map) => Ok(map),
        other => Err(format!(
            "'{key}' is a {}, not an object; refusing to replace it",
            kind_of(other)
        )),
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn backup_if_exists(path: &Path) -> Result<(), Box<dyn Error>> {
    if !path.exists() {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    fs::copy(path, &backup).map_err(|error| format!("{}: {error}", backup.display()))?;
    println!("backed up {} to {}", path.display(), backup.display());
    Ok(())
}

fn write_json_object(path: &Path, root: &Map<String, Value>) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    // Sorting keeps rewrites of an existing configuration reviewable as a diff.
    let ordered: BTreeMap<&String, &Value> = root.iter().collect();
    let mut text = serde_json::to_string_pretty(&ordered)?;
    text.push('\n');
    fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| {
            if part.is_empty()
                || part
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || "-_./:=@".contains(c)))
            {
                format!("'{}'", part.replace('\'', r"'\''"))
            } else {
                part.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("websift-setup-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options(name: &str) -> SetupOptions {
        SetupOptions {
            // `--command` is given explicitly so no test depends on PATH.
            command: Some(
                std::env::current_exe()
                    .expect("test binary")
                    .display()
                    .to_string(),
            ),
            name: name.to_owned(),
            ..SetupOptions::default()
        }
    }

    /// Drives the same entry point `websift setup --client claude-code` reaches, with the
    /// configuration redirected away from the real one.
    fn install_claude_code(path: &Path, options: &SetupOptions) -> Result<(), Box<dyn Error>> {
        run(&SetupOptions {
            client: Some(Client::ClaudeCode),
            config: Some(path.to_path_buf()),
            ..options.clone()
        })
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("config")).expect("json")
    }

    #[test]
    fn install_into_a_missing_file_creates_it_holding_only_our_entry() {
        let dir = TempDir::new("create");
        let path = dir.join("claude.json");

        install_claude_code(&path, &options("websift")).expect("install");

        let config = read(&path);
        assert!(config["mcpServers"]["websift"]["command"].is_string());
        assert_eq!(
            config["mcpServers"]["websift"]["args"],
            json!(["mcp", "--profile", "default"])
        );
        assert_eq!(config["mcpServers"].as_object().expect("map").len(), 1);
    }

    #[test]
    fn install_preserves_unrelated_keys_and_unrelated_servers() {
        let dir = TempDir::new("preserve");
        let path = dir.join("claude.json");
        fs::write(
            &path,
            r#"{"theme":"dark","mcpServers":{"other":{"command":"other"}}}"#,
        )
        .expect("seed");

        install_claude_code(&path, &options("websift")).expect("install");

        let config = read(&path);
        assert_eq!(config["theme"], json!("dark"));
        assert_eq!(config["mcpServers"]["other"]["command"], json!("other"));
        assert!(config["mcpServers"]["websift"].is_object());
    }

    #[test]
    fn an_existing_entry_is_refused_without_force_and_replaced_with_it() {
        let dir = TempDir::new("force");
        let path = dir.join("claude.json");
        fs::write(&path, r#"{"mcpServers":{"websift":{"command":"stale"}}}"#).expect("seed");

        let error = install_claude_code(&path, &options("websift")).expect_err("refusal");
        assert!(error.to_string().contains("--force"), "{error}");
        assert_eq!(
            read(&path)["mcpServers"]["websift"]["command"],
            json!("stale")
        );

        let forced = SetupOptions {
            force: true,
            ..options("websift")
        };
        install_claude_code(&path, &forced).expect("forced install");
        assert_ne!(
            read(&path)["mcpServers"]["websift"]["command"],
            json!("stale")
        );
    }

    #[test]
    fn an_existing_config_is_backed_up_before_being_rewritten() {
        let dir = TempDir::new("backup");
        let path = dir.join("claude.json");
        fs::write(&path, r#"{"theme":"dark"}"#).expect("seed");

        install_claude_code(&path, &options("websift")).expect("install");

        let backup = dir.join("claude.json.bak");
        assert_eq!(
            fs::read_to_string(backup).expect("backup"),
            r#"{"theme":"dark"}"#
        );
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let dir = TempDir::new("dry");
        let path = dir.join("claude.json");
        let dry = SetupOptions {
            dry_run: true,
            ..options("websift")
        };

        install_claude_code(&path, &dry).expect("dry run");

        assert!(!path.exists(), "dry run created {}", path.display());
    }

    #[test]
    fn a_config_with_jsonc_comments_is_parsed_rather_than_rejected() {
        let dir = TempDir::new("jsonc");
        let path = dir.join("opencode.jsonc");
        fs::write(
            &path,
            "{\n  // the url is not a comment: http://example.com\n  \"theme\": \"dark\" /* trailing */\n}\n",
        )
        .expect("seed");

        let root = load_json_object(&path).expect("parse");

        assert_eq!(root["theme"], json!("dark"));
    }

    #[test]
    fn a_url_inside_a_string_survives_comment_stripping() {
        assert_eq!(
            strip_comments(r#"{"a":"http://x/y"}"#),
            r#"{"a":"http://x/y"}"#
        );
    }

    #[test]
    fn a_non_object_at_the_target_key_is_rejected_instead_of_replaced() {
        let dir = TempDir::new("nonobject");
        let path = dir.join("claude.json");
        fs::write(&path, r#"{"mcpServers":"nope"}"#).expect("seed");

        let error = install_claude_code(&path, &options("websift")).expect_err("refusal");

        assert!(error.to_string().contains("refusing to replace"), "{error}");
        assert_eq!(read(&path)["mcpServers"], json!("nope"));
    }

    #[test]
    fn unparsable_config_reports_the_path_instead_of_overwriting_it() {
        let dir = TempDir::new("broken");
        let path = dir.join("claude.json");
        fs::write(&path, "{ not json").expect("seed");

        let error = install_claude_code(&path, &options("websift")).expect_err("refusal");

        assert!(error.to_string().contains("not valid JSON"), "{error}");
        assert_eq!(fs::read_to_string(&path).expect("untouched"), "{ not json");
    }

    #[test]
    fn opencode_entry_uses_a_local_command_array_under_the_mcp_key() {
        let spec = server_spec(Client::OpenCode, "websift", "/bin/websift", "work");
        assert_eq!(
            spec.entry,
            json!({
                "type": "local",
                "command": ["/bin/websift", "mcp", "--profile", "work"],
                "enabled": true,
            })
        );
        assert!(spec.snippet["mcp"]["websift"].is_object());
    }

    #[test]
    fn zed_entry_uses_the_context_servers_shape() {
        let spec = server_spec(Client::Zed, "websift", "/bin/websift", "work");
        assert_eq!(spec.entry["source"], json!("custom"));
        assert!(spec.snippet["context_servers"]["websift"].is_object());
    }

    #[test]
    fn copilot_cli_entry_declares_the_stdio_transport() {
        let spec = server_spec(Client::CopilotCli, "websift", "/bin/websift", "work");
        assert_eq!(spec.entry["type"], json!("stdio"));
    }

    #[test]
    fn delegated_steps_pass_the_profile_through_to_the_client_cli() {
        let steps = delegated_steps(
            Client::Codex,
            "codex",
            "websift",
            "/bin/websift",
            "work",
            false,
        );
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0],
            vec![
                "codex",
                "mcp",
                "add",
                "websift",
                "--",
                "/bin/websift",
                "mcp",
                "--profile",
                "work"
            ]
        );
    }

    #[test]
    fn forcing_a_delegated_client_removes_the_entry_before_adding_it() {
        let steps = delegated_steps(
            Client::ClaudeCodeCli,
            "claude",
            "websift",
            "/bin/websift",
            "work",
            true,
        );
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0][1..3], ["mcp", "remove"]);
        assert_eq!(steps[1][1..3], ["mcp", "add"]);
    }

    #[test]
    fn vscode_receives_the_entry_as_one_json_argument() {
        let steps = delegated_steps(
            Client::Vscode,
            "code",
            "websift",
            "/bin/websift",
            "work",
            false,
        );
        assert_eq!(steps[0][1], "--add-mcp");
        let entry: Value = serde_json::from_str(&steps[0][2]).expect("json argument");
        assert_eq!(entry["name"], json!("websift"));
        assert_eq!(entry["args"], json!(["mcp", "--profile", "work"]));
    }

    #[test]
    fn every_client_id_round_trips_through_parse() {
        for client in Client::ALL {
            assert_eq!(Client::parse(client.id()), Ok(client));
        }
        assert!(Client::parse("nope").is_err());
    }

    #[test]
    fn a_profile_that_would_escape_its_filename_is_rejected() {
        assert_eq!(normalize_profile(" work "), Ok("work".to_owned()));
        assert!(normalize_profile("../etc").is_err());
        assert!(normalize_profile("").is_err());
        assert!(normalize_profile(&"a".repeat(65)).is_err());
    }

    #[test]
    fn branding_collapses_to_plain_text_when_stdout_is_not_a_terminal() {
        // Under `cargo test` stdout is a pipe, which is exactly the redirected case.
        let banner = theme::banner();
        assert!(!banner.contains('\x1b'), "{banner:?}");
        assert!(banner.contains("W E B S I F T"));
    }

    #[test]
    fn a_panel_pads_every_row_to_the_width_of_the_widest_line() {
        let panel = theme::panel(
            &["short".to_owned(), "a much longer line".to_owned()],
            "planned changes",
            theme::ACCENT,
        );
        let widths: Vec<usize> = panel.lines().map(|line| line.chars().count()).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "{widths:?}"
        );
    }

    #[test]
    fn the_preview_lines_name_the_file_and_whether_the_entry_is_replaced() {
        let dir = TempDir::new("lines");
        let path = dir.join("claude.json");
        fs::write(&path, r#"{"mcpServers":{"websift":{"command":"stale"}}}"#).expect("seed");

        let forced = SetupOptions {
            client: Some(Client::ClaudeCode),
            config: Some(path.clone()),
            force: true,
            ..options("websift")
        };
        let installation = resolve(Client::ClaudeCode, "default", &forced).expect("resolve");

        let lines = installation.lines();
        assert!(lines[0].starts_with("replace"), "{lines:?}");
        assert!(lines[1].contains("claude.json"), "{lines:?}");
    }

    #[test]
    fn shell_join_quotes_arguments_that_the_shell_would_split() {
        assert_eq!(shell_join(&["a".to_owned(), "b c".to_owned()]), "a 'b c'");
        assert_eq!(shell_join(&["/usr/bin/x".to_owned()]), "/usr/bin/x");
    }
}
