//! Self-update against published GitHub releases.
//!
//! Release metadata and binaries are untrusted input. A downloaded binary is verified against the
//! checksum published beside it and is only allowed to replace the running executable after that
//! check passes. The same public-URL and DNS policy used for retrieval applies here, so an update
//! cannot be pointed at a private address.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{
    Client, StatusCode,
    header::{ACCEPT, USER_AGENT},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::policy::{PublicUrl, SystemDnsResolver, ValidatingDnsResolver};

const REPO: &str = "suiflex/websift";
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ADDRESSES: usize = 16;
const MAX_REDIRECTS: usize = 5;
/// Generous ceiling for one binary; the release assets are a few megabytes.
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const CLIENT_USER_AGENT: &str = concat!("websift/", env!("CARGO_PKG_VERSION"));

/// Stable failures returned by the updater.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("update request failed: {0}")]
    Transport(String),
    #[error("update request timed out: {0}")]
    Timeout(String),
    #[error("release lookup returned status {0}")]
    Status(StatusCode),
    #[error("release metadata was invalid: {0}")]
    InvalidMetadata(String),
    #[error("this platform has no published binary: {0}")]
    UnsupportedPlatform(String),
    #[error("release {version} does not publish {asset}")]
    MissingAsset { version: String, asset: String },
    #[error("checksum mismatch for {asset}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        asset: String,
        expected: String,
        actual: String,
    },
    #[error("download exceeded maximum size of {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("could not replace the running executable: {0}")]
    Replace(#[source] io::Error),
}

/// Result of comparing the running build against the latest published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatus {
    pub current: String,
    /// Release tag exactly as published, which is what the download URLs use.
    pub latest_tag: String,
    pub available: bool,
}

impl UpdateStatus {
    /// Latest version in the same form as `current`, so the two can be compared as strings.
    ///
    /// The tag carries a `v` prefix that the crate version does not; reporting both forms
    /// unchanged would make an equal pair look different to a caller.
    #[must_use]
    pub fn latest_version(&self) -> &str {
        self.latest_tag.trim_start_matches('v')
    }
}

/// The running version, without a leading `v`.
#[must_use]
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Rust target triple for the running build, derived from compile-time constants.
///
/// # Errors
///
/// Returns an error on a platform with no published release asset.
pub fn target_triple() -> Result<String, UpdateError> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(UpdateError::UnsupportedPlatform(format!("arch {other}"))),
    };
    let system = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => return Err(UpdateError::UnsupportedPlatform(format!("os {other}"))),
    };
    Ok(format!("{arch}-{system}"))
}

/// Asset name published for one release and target.
#[must_use]
pub fn asset_name(tag: &str, triple: &str) -> String {
    let suffix = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    format!("websift-{tag}-{triple}{suffix}")
}

/// Compare two versions by numeric release precedence.
///
/// A missing or non-numeric component sorts as zero, so a malformed tag can never claim to be
/// newer than a well-formed one.
#[must_use]
pub fn is_newer(candidate: &str, current: &str) -> bool {
    parts(candidate) > parts(current)
}

fn parts(version: &str) -> [u64; 3] {
    let trimmed = version.trim().trim_start_matches('v');
    let mut out = [0_u64; 3];
    for (slot, piece) in out.iter_mut().zip(trimmed.split('.')) {
        // A pre-release suffix such as `1.2.3-rc1` contributes only its numeric prefix.
        let digits: String = piece.chars().take_while(char::is_ascii_digit).collect();
        *slot = digits.parse().unwrap_or(0);
    }
    out
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
}

/// Bounded client for release metadata and binary downloads.
#[derive(Debug, Clone)]
pub struct Updater {
    client: Client,
    timeout: Duration,
}

impl Updater {
    /// Build an updater bound by `timeout`, reusing the shared public-address policy.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(timeout: Duration) -> Result<Self, UpdateError> {
        let resolver = ValidatingDnsResolver::new(
            Arc::new(SystemDnsResolver),
            RESOLVE_TIMEOUT.min(timeout),
            MAX_ADDRESSES,
        );
        let client = Client::builder()
            .dns_resolver(Arc::new(resolver))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                // Release downloads redirect to object storage, which must still be public HTTPS.
                let Some(previous) = attempt.previous().last() else {
                    return attempt.stop();
                };
                let is_downgrade = previous.scheme() == "https" && attempt.url().scheme() == "http";
                if attempt.previous().len() > MAX_REDIRECTS
                    || is_downgrade
                    || PublicUrl::parse(attempt.url().as_str()).is_err()
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(|error| UpdateError::Transport(error.to_string()))?;
        Ok(Self { client, timeout })
    }

    /// Look up the latest published release tag and compare it with the running build.
    ///
    /// # Errors
    ///
    /// Returns transport, status, or metadata failures.
    pub async fn check(&self) -> Result<UpdateStatus, UpdateError> {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let body = self
            .get(&url, MAX_METADATA_BYTES, "application/vnd.github+json")
            .await?;
        let release: Release = serde_json::from_slice(&body)
            .map_err(|error| UpdateError::InvalidMetadata(error.to_string()))?;
        let latest = release.tag_name.trim().to_owned();
        if latest.is_empty() {
            return Err(UpdateError::InvalidMetadata("empty tag name".to_owned()));
        }
        Ok(UpdateStatus {
            current: current_version().to_owned(),
            available: is_newer(&latest, current_version()),
            latest_tag: latest,
        })
    }

    /// Download the release binary for this platform and verify its published checksum.
    ///
    /// # Errors
    ///
    /// Returns transport, missing-asset, size, or checksum failures.
    pub async fn download_verified(&self, tag: &str) -> Result<Vec<u8>, UpdateError> {
        let triple = target_triple()?;
        let asset = asset_name(tag, &triple);
        let base = format!("https://github.com/{REPO}/releases/download/{tag}");

        let checksum = self
            .get(
                &format!("{base}/{asset}.sha256"),
                MAX_METADATA_BYTES,
                "text/plain",
            )
            .await
            .map_err(|error| match error {
                UpdateError::Status(_) => UpdateError::MissingAsset {
                    version: tag.to_owned(),
                    asset: format!("{asset}.sha256"),
                },
                other => other,
            })?;
        let expected = expected_checksum(&checksum)?;

        let binary = self
            .get(
                &format!("{base}/{asset}"),
                MAX_DOWNLOAD_BYTES,
                "application/octet-stream",
            )
            .await
            .map_err(|error| match error {
                UpdateError::Status(_) => UpdateError::MissingAsset {
                    version: tag.to_owned(),
                    asset: asset.clone(),
                },
                other => other,
            })?;

        let actual = format!("{:x}", Sha256::digest(&binary));
        if actual != expected {
            return Err(UpdateError::ChecksumMismatch {
                asset,
                expected,
                actual,
            });
        }
        Ok(binary)
    }

    async fn get(&self, url: &str, limit: u64, accept: &str) -> Result<Vec<u8>, UpdateError> {
        let url = PublicUrl::parse(url)
            .map_err(|error| UpdateError::InvalidMetadata(format!("{error:?}")))?;
        let response = self
            .client
            .get(url.as_str())
            .header(USER_AGENT, CLIENT_USER_AGENT)
            .header(ACCEPT, accept)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    UpdateError::Timeout(error.to_string())
                } else {
                    UpdateError::Transport(error.to_string())
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(UpdateError::Status(status));
        }
        if response.content_length().is_some_and(|size| size > limit) {
            return Err(UpdateError::TooLarge { limit });
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| UpdateError::Transport(error.to_string()))?;
            let new_len = body
                .len()
                .checked_add(chunk.len())
                .ok_or(UpdateError::TooLarge { limit })?;
            if u64::try_from(new_len).map_or(true, |size| size > limit) {
                return Err(UpdateError::TooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// Read the digest out of a `sha256sum` style line.
fn expected_checksum(body: &[u8]) -> Result<String, UpdateError> {
    let text = String::from_utf8_lossy(body);
    let digest = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UpdateError::InvalidMetadata(
            "checksum file did not contain a sha256 digest".to_owned(),
        ));
    }
    Ok(digest)
}

/// Replace `destination` with `binary`, keeping the running executable usable throughout.
///
/// The new file is written beside the destination so the final step is a same-filesystem rename.
/// Windows cannot overwrite a running image, so the current file is moved aside first and removed
/// on a later run.
///
/// # Errors
///
/// Returns an error if the temporary file cannot be written or the rename fails.
pub fn replace_executable(destination: &Path, binary: &[u8]) -> Result<(), UpdateError> {
    let directory = destination.parent().unwrap_or_else(|| Path::new("."));
    let staged = directory.join(format!("websift-update-{}.tmp", std::process::id()));
    std::fs::write(&staged, binary).map_err(UpdateError::Replace)?;

    // Every failure past this point must remove the staging file: a partially applied update
    // otherwise leaves a full copy of the binary beside the real one on each attempt.
    let result = stage_into_place(&staged, destination);
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn stage_into_place(staged: &Path, destination: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))
            .map_err(UpdateError::Replace)?;
    }

    if cfg!(windows) {
        // Windows refuses to overwrite a running image, so the current one is moved aside.
        let retired: PathBuf = destination.with_extension("old");
        let _ = std::fs::remove_file(&retired);
        if destination.exists() {
            std::fs::rename(destination, &retired).map_err(UpdateError::Replace)?;
        }
    }

    std::fs::rename(staged, destination).map_err(UpdateError::Replace)
}

#[cfg(test)]
mod tests {
    use super::{asset_name, expected_checksum, is_newer, replace_executable, target_triple};

    #[test]
    fn orders_versions_by_release_precedence() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        // A malformed tag must never win against a well-formed current version.
        assert!(!is_newer("latest", "0.1.0"));
        assert!(!is_newer("", "0.1.0"));
    }

    #[test]
    fn builds_platform_specific_asset_names() {
        assert_eq!(
            asset_name("v0.1.0", "x86_64-unknown-linux-gnu"),
            "websift-v0.1.0-x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            asset_name("v0.1.0", "aarch64-pc-windows-msvc"),
            "websift-v0.1.0-aarch64-pc-windows-msvc.exe"
        );
        assert!(target_triple().is_ok());
    }

    #[test]
    fn rejects_anything_that_is_not_a_sha256_digest() {
        let digest = "a".repeat(64);
        assert_eq!(
            expected_checksum(format!("{digest}  websift-v0.1.0-x86_64-apple-darwin").as_bytes())
                .unwrap(),
            digest
        );
        assert!(expected_checksum(b"").is_err());
        assert!(expected_checksum(b"not-a-digest  file").is_err());
        assert!(expected_checksum("z".repeat(64).as_bytes()).is_err());
    }

    #[test]
    fn replaces_the_target_file_in_place() {
        let directory = std::env::temp_dir().join(format!("websift-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("websift");
        std::fs::write(&destination, b"old").unwrap();

        replace_executable(&destination, b"new").unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        // No staging file may be left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("update"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn leaves_no_staging_file_when_the_replacement_fails() {
        let directory =
            std::env::temp_dir().join(format!("websift-test-fail-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        // Renaming onto a directory fails, standing in for any failure after staging.
        let destination = directory.join("occupied");
        std::fs::create_dir_all(&destination).unwrap();

        assert!(replace_executable(&destination, b"new").is_err());

        let staged: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("update"))
            .collect();
        assert!(
            staged.is_empty(),
            "a failed update must not leave a copy of the binary behind"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn reports_the_latest_version_in_the_same_form_as_the_current_one() {
        let status = super::UpdateStatus {
            current: "0.1.1".to_owned(),
            latest_tag: "v0.1.1".to_owned(),
            available: false,
        };
        assert_eq!(status.latest_version(), status.current);
        assert_eq!(status.latest_tag, "v0.1.1");
    }
}
