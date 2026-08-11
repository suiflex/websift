//! Validated process configuration.

use std::{
    env,
    path::{Path, PathBuf},
};

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_MAX_RESULTS: u32 = 10;
const DEFAULT_MAX_BYTES: u64 = 2_000_000;
const DEFAULT_CRAWL_CONCURRENCY: u16 = 4;
const DEFAULT_SPOOL_ROOT: &str = "/tmp/websift-spool";
const DEFAULT_PER_HOST_CONCURRENCY: u16 = 2;
const DEFAULT_CACHE_TTL_MS: u64 = 900_000;
const DEFAULT_DEEP_SEARCH_BUDGET_MS: u64 = 60_000;
const DATA_DIR_ENV: &str = "WEBSIFT_DATA_DIR";

/// Configuration selected when the process starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub profile: String,
    pub searxng_url: Option<String>,
    pub timeout_ms: u64,
    pub max_results: u32,
    pub max_bytes: u64,
    pub crawl_concurrency: u16,
    pub per_host_concurrency: u16,
    /// Page-cache lifetime in milliseconds. `0` disables the cache.
    pub cache_ttl_ms: u64,
    /// Wall-clock ceiling for one `web_deep_search` operation.
    pub deep_search_budget_ms: u64,
    /// Whether a failing configured instance may fall back to the built-in public backend.
    ///
    /// Off by default: a caller who configures a private instance is choosing not to send
    /// queries to a public engine, and a transient failure must not quietly undo that choice.
    pub search_fallback: bool,
    pub browser: BrowserMode,
    pub spool_root: PathBuf,
    pub worker_program: PathBuf,
    pub worker_args: Vec<String>,
    pub data_dir: PathBuf,
}

/// Browser-worker policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    Auto,
    Enabled,
    Disabled,
}

impl Config {
    /// Load and validate environment-backed configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when an environment variable is malformed or outside its supported bounds.
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_with_profile(None)
    }

    /// Load environment configuration, optionally overriding its profile from the CLI.
    ///
    /// # Errors
    ///
    /// Returns an error when either the environment or CLI profile is invalid.
    pub fn from_env_with_profile(profile_override: Option<&str>) -> Result<Self, String> {
        let mut config = Self::from_lookup(|key| env::var(key).ok())?;
        if let Some(profile) = profile_override {
            config.profile = crate::application::RuntimeStatus::new(profile)
                .map_err(str::to_owned)?
                .profile;
        }
        Ok(config)
    }

    pub(crate) fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let profile = lookup("WEBSIFT_PROFILE").unwrap_or_else(|| "default".to_owned());
        let profile = crate::application::RuntimeStatus::new(&profile)
            .map_err(str::to_owned)?
            .profile;
        let searxng_url = lookup("WEBSIFT_SEARXNG_URL")
            .map(|value| {
                crate::policy::PublicUrl::parse(&value)
                    .map(|url| url.as_str().to_owned())
                    .map_err(|error| format!("WEBSIFT_SEARXNG_URL is invalid: {error:?}"))
            })
            .transpose()?;
        let timeout_ms = parse_timeout_ms(&mut lookup)?;
        let max_results = parse_u32(
            &mut lookup,
            "WEBSIFT_MAX_RESULTS",
            DEFAULT_MAX_RESULTS,
            1,
            50,
        )?;
        let max_bytes = parse_u64(
            &mut lookup,
            "WEBSIFT_MAX_BYTES",
            DEFAULT_MAX_BYTES,
            1,
            100_000_000,
        )?;
        let crawl_concurrency = parse_u16(
            &mut lookup,
            "WEBSIFT_CRAWL_CONCURRENCY",
            DEFAULT_CRAWL_CONCURRENCY,
            1,
            32,
        )?;
        let per_host_concurrency = parse_u16(
            &mut lookup,
            "WEBSIFT_PER_HOST_CONCURRENCY",
            DEFAULT_PER_HOST_CONCURRENCY,
            1,
            32,
        )?;
        let cache_ttl_ms = parse_u64(
            &mut lookup,
            "WEBSIFT_CACHE_TTL_MS",
            DEFAULT_CACHE_TTL_MS,
            0,
            86_400_000,
        )?;
        let deep_search_budget_ms = parse_u64(
            &mut lookup,
            "WEBSIFT_DEEP_SEARCH_BUDGET_MS",
            DEFAULT_DEEP_SEARCH_BUDGET_MS,
            1_000,
            600_000,
        )?;
        let search_fallback = parse_flag(&mut lookup, "WEBSIFT_SEARCH_FALLBACK")?;
        let browser = match lookup("WEBSIFT_BROWSER").as_deref().unwrap_or("auto") {
            "auto" => BrowserMode::Auto,
            "enabled" => BrowserMode::Enabled,
            "disabled" => BrowserMode::Disabled,
            value => return Err(format!("WEBSIFT_BROWSER has unsupported value: {value}")),
        };
        let spool_root = lookup("WEBSIFT_SPOOL_ROOT")
            .map_or_else(|| PathBuf::from(DEFAULT_SPOOL_ROOT), PathBuf::from);
        validate_path("WEBSIFT_SPOOL_ROOT", &spool_root)?;
        let worker_program =
            lookup("WEBSIFT_WORKER_PROGRAM").map_or_else(|| PathBuf::from("node"), PathBuf::from);
        validate_path("WEBSIFT_WORKER_PROGRAM", &worker_program)?;
        let worker_args = lookup("WEBSIFT_WORKER_ARGS").map_or_else(
            || {
                vec![
                    "--experimental-strip-types".to_owned(),
                    "browser-worker/src/main.ts".to_owned(),
                ]
            },
            |value| value.split('\u{1f}').map(str::to_owned).collect(),
        );
        if worker_args.len() > 16 || worker_args.iter().any(|arg| arg.len() > 1024) {
            return Err("WEBSIFT_WORKER_ARGS is invalid".to_owned());
        }
        let data_dir = lookup(DATA_DIR_ENV).map_or_else(default_data_dir, PathBuf::from);
        validate_path(DATA_DIR_ENV, &data_dir)?;

        Ok(Self {
            profile,
            searxng_url,
            timeout_ms,
            max_results,
            max_bytes,
            crawl_concurrency,
            per_host_concurrency,
            cache_ttl_ms,
            deep_search_budget_ms,
            search_fallback,
            browser,
            spool_root,
            worker_program,
            worker_args,
            data_dir,
        })
    }
}

fn default_data_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        env::var_os("HOME").map_or_else(
            || PathBuf::from("/tmp/websift"),
            |home| PathBuf::from(home).join("Library/Application Support/websift"),
        )
    } else if cfg!(target_os = "windows") {
        env::var_os("LOCALAPPDATA").map_or_else(
            || PathBuf::from("websift"),
            |dir| PathBuf::from(dir).join("websift"),
        )
    } else {
        env::var_os("XDG_DATA_HOME")
            .or_else(|| {
                env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local/share").into_os_string())
            })
            .map_or_else(
                || PathBuf::from("/tmp/websift"),
                |dir| PathBuf::from(dir).join("websift"),
            )
    }
}

fn validate_path(key: &str, path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.to_string_lossy().len() > 512 {
        return Err(format!("{key} is invalid"));
    }
    Ok(())
}

fn parse_timeout_ms(lookup: &mut impl FnMut(&str) -> Option<String>) -> Result<u64, String> {
    let key = if lookup("WEBSIFT_TIMEOUT").is_some() {
        "WEBSIFT_TIMEOUT"
    } else {
        "WEBSIFT_TIMEOUT_MS"
    };
    let Some(value) = lookup(key) else {
        return Ok(DEFAULT_TIMEOUT_MS);
    };
    if let Some(seconds) = value.strip_suffix('s') {
        let seconds = seconds
            .parse::<u64>()
            .map_err(|_| format!("{key} must be a duration"))?;
        return seconds
            .checked_mul(1_000)
            .filter(|milliseconds| (1..=300_000).contains(milliseconds))
            .ok_or_else(|| format!("{key} must be between 1ms and 300s"));
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{key} must be a number or duration"))?;
    if !(1..=300_000).contains(&parsed) {
        return Err(format!("{key} must be between 1ms and 300s"));
    }
    Ok(parsed)
}

/// Parse an off-by-default boolean switch.
fn parse_flag(lookup: &mut impl FnMut(&str) -> Option<String>, key: &str) -> Result<bool, String> {
    match lookup(key).as_deref() {
        None | Some("0" | "false") => Ok(false),
        Some("1" | "true") => Ok(true),
        Some(value) => Err(format!("{key} must be 0, 1, true, or false: {value}")),
    }
}

fn parse_u64(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    parse_number(lookup, key, default, min, max)
}

fn parse_u32(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    key: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32, String> {
    parse_number(lookup, key, default, min, max)
}

fn parse_u16(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    key: &str,
    default: u16,
    min: u16,
    max: u16,
) -> Result<u16, String> {
    parse_number(lookup, key, default, min, max)
}

#[allow(clippy::needless_pass_by_value)]
fn parse_number<T>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    key: &str,
    default: T,
    min: T,
    max: T,
) -> Result<T, String>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display + Copy,
{
    let Some(value) = lookup(key) else {
        return Ok(default);
    };
    let parsed = value
        .parse::<T>()
        .map_err(|_| format!("{key} must be a number"))?;
    if parsed < min || parsed > max {
        return Err(format!("{key} must be between {min} and {max}"));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn search_fallback_is_off_unless_explicitly_requested() {
        use super::Config;
        assert!(!Config::from_lookup(|_| None).unwrap().search_fallback);
        assert!(
            Config::from_lookup(|key| (key == "WEBSIFT_SEARCH_FALLBACK").then(|| "1".to_owned()))
                .unwrap()
                .search_fallback
        );
        assert!(
            !Config::from_lookup(
                |key| (key == "WEBSIFT_SEARCH_FALLBACK").then(|| "false".to_owned())
            )
            .unwrap()
            .search_fallback
        );
        assert!(
            Config::from_lookup(|key| (key == "WEBSIFT_SEARCH_FALLBACK").then(|| "yes".to_owned()))
                .is_err()
        );
    }

    use std::path::PathBuf;

    use super::{BrowserMode, Config};

    fn config(values: &[(&str, &str)]) -> Result<Config, String> {
        Config::from_lookup(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn applies_safe_defaults() {
        let value = config(&[]).unwrap();
        assert_eq!(value.profile, "default");
        assert_eq!(value.browser, BrowserMode::Auto);
        assert_eq!(value.max_results, 10);
    }

    #[test]
    fn rejects_out_of_bounds_values() {
        assert!(config(&[("WEBSIFT_MAX_RESULTS", "51")]).is_err());
        assert!(config(&[("WEBSIFT_BROWSER", "maybe")]).is_err());
        assert!(config(&[("WEBSIFT_SEARXNG_URL", "http://127.0.0.1")]).is_err());
    }

    #[test]
    fn accepts_duration_timeout() {
        assert_eq!(
            config(&[("WEBSIFT_TIMEOUT", "30s")]).unwrap().timeout_ms,
            30_000
        );
    }

    #[test]
    fn parses_explicit_data_directory_and_rejects_empty_path() {
        assert_eq!(
            config(&[("WEBSIFT_DATA_DIR", "/var/lib/websift")])
                .unwrap()
                .data_dir,
            PathBuf::from("/var/lib/websift")
        );
        assert!(config(&[("WEBSIFT_DATA_DIR", "")]).is_err());
    }

    #[test]
    fn cli_profile_overrides_environment_profile_only() {
        let config = Config::from_lookup(|key| match key {
            "WEBSIFT_PROFILE" => Some("environment".to_owned()),
            "WEBSIFT_MAX_RESULTS" => Some("25".to_owned()),
            _ => None,
        })
        .unwrap();
        assert_eq!(config.profile, "environment");
        assert_eq!(config.max_results, 25);

        let mut config = config;
        config.profile = crate::application::RuntimeStatus::new("cli")
            .unwrap()
            .profile;
        assert_eq!(config.profile, "cli");
        assert_eq!(config.max_results, 25);
    }
}
