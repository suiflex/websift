//! Transport-independent application operations.

use schemars::JsonSchema;
use serde::Serialize;

/// Current process capabilities exposed to installers and operators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RuntimeStatus {
    /// Package version.
    pub version: &'static str,
    /// Startup-selected visibility namespace.
    pub profile: String,
}

impl RuntimeStatus {
    /// Build status from process configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when `profile` is empty, oversized, or unsafe as a storage key.
    pub fn new(profile: &str) -> Result<Self, &'static str> {
        let profile = profile.trim();
        if profile.is_empty()
            || profile.len() > 64
            || !profile
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("profile must be 1-64 ASCII letters, digits, '-' or '_'");
        }

        Ok(Self {
            version: env!("CARGO_PKG_VERSION"),
            profile: profile.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeStatus;

    #[test]
    fn profile_is_bounded_and_safe_for_future_storage_keys() {
        assert!(RuntimeStatus::new("codex_1").is_ok());
        assert!(RuntimeStatus::new("../claude").is_err());
        assert!(RuntimeStatus::new("").is_err());
        assert!(RuntimeStatus::new(&"a".repeat(65)).is_err());
    }
}
