//! Structured operational logging.
//!
//! One JSON object per event on stderr, because stdout carries the MCP protocol. Events record
//! what an operation did — name, duration, status, counters — and deliberately omit queries,
//! URLs, and page content, which may carry caller or third-party data.

use std::{
    io::Write,
    sync::OnceLock,
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};

/// `WEBSIFT_LOG=off` silences events; any other value keeps the default JSON lines.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("WEBSIFT_LOG").map_or(true, |value| !value.eq_ignore_ascii_case("off"))
    })
}

/// Emit one event. Field values must never contain queries, URLs, or page content.
pub fn event(name: &str, status: &str, duration: Duration, fields: &[(&str, Value)]) {
    if !enabled() {
        return;
    }
    let mut object = Map::new();
    object.insert("event".to_owned(), json!(name));
    object.insert("status".to_owned(), json!(status));
    object.insert(
        "duration_ms".to_owned(),
        json!(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
    );
    for (key, value) in fields {
        object.insert((*key).to_owned(), value.clone());
    }
    let line = Value::Object(object).to_string();
    // Logging must never take down an operation, so a closed or blocked stderr is ignored.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

/// Measures one operation for a later [`event`] call.
#[derive(Debug)]
pub struct Timer(Instant);

impl Timer {
    #[must_use]
    pub fn start() -> Self {
        Self(Instant::now())
    }
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::{Timer, event};
    use std::time::Duration;

    #[test]
    fn emitting_an_event_never_panics_and_reports_elapsed_time() {
        let timer = Timer::start();
        event(
            "test_operation",
            "ok",
            timer.elapsed(),
            &[("result_count", serde_json::json!(3))],
        );
        assert!(timer.elapsed() < Duration::from_secs(5));
    }
}
