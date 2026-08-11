//! Bounded JSONL supervisor for the browser worker.
//!
//! The supervisor owns one worker process at a time. It validates the worker's
//! hello frame before sending requests and keeps all worker output off the MCP
//! process stdout (worker stderr is discarded).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};

/// `CREATE_NO_WINDOW`: start a console child without allocating a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const PROTOCOL_VERSION: u8 = 1;
const MAX_FRAME_BYTES: usize = 1_048_576;
const MAX_ARTIFACTS: usize = 7;
const MAX_ARTIFACT_BYTES: u64 = 100_000;

#[derive(Debug, Clone)]
pub struct Spool {
    root: PathBuf,
    id: String,
}

impl Spool {
    /// Create a bounded spool directory and write the input artifact.
    ///
    /// # Errors
    ///
    /// Returns an error when the input exceeds the artifact bound or the spool directory or input file cannot be created.
    pub fn create(root: impl AsRef<Path>, input: &[u8]) -> Result<Self, WorkerError> {
        if input.len() > usize::try_from(MAX_ARTIFACT_BYTES).unwrap_or(usize::MAX) {
            return Err(WorkerError::Protocol(
                "spool input exceeds bound".to_owned(),
            ));
        }
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(WorkerError::Io)?;
        let id = format!(
            "spool-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let path = root.join(&id);
        fs::create_dir(&path).map_err(WorkerError::Io)?;
        fs::write(path.join("input.html"), input).map_err(WorkerError::Io)?;
        Ok(Self { root, id })
    }
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.id)
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("failed to spawn worker: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("worker protocol error: {0}")]
    Protocol(String),
    #[error("worker request timed out after {0}ms")]
    Timeout(u64),
    #[error("worker request was cancelled")]
    Cancelled,
    #[error("worker exited before responding")]
    Eof,
    #[error("worker I/O failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("worker returned {code}: {message}")]
    Remote {
        code: String,
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(rename = "type")]
    pub message_type: String,
    pub protocol_version: u8,
    pub request_id: String,
    pub operation: Operation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub deadline_ms: u64,
    pub spool_id: String,
    pub options: Options,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Extract,
    Render,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub formats: Vec<String>,
    pub only_main_content: bool,
    pub wait_for_ms: u64,
    pub max_output_chars: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    #[serde(rename = "type")]
    pub message_type: String,
    pub protocol_version: u8,
    pub worker_version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Response {
    #[serde(rename = "result")]
    Result(ResultMessage),
    #[serde(rename = "error")]
    Error(ErrorMessage),
    #[serde(rename = "heartbeat")]
    Heartbeat {
        protocol_version: u8,
        request_id: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultMessage {
    pub protocol_version: u8,
    pub request_id: String,
    pub status: String,
    pub artifacts: Vec<Artifact>,

    pub warnings: Vec<String>,
    pub timing_ms: serde_json::Map<String, serde_json::Value>,
    pub final_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub kind: String,
    pub path: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorMessage {
    protocol_version: u8,
    request_id: String,
    status: String,
    code: String,
    message: String,
    retryable: bool,
}

/// Validate a hello frame without starting a process.
///
/// # Errors
///
/// Returns an error when the frame is malformed or advertises an unsupported protocol or capability.
pub fn validate_hello_frame(frame: &str) -> Result<Hello, WorkerError> {
    let hello: Hello = serde_json::from_str(frame)
        .map_err(|error| WorkerError::Protocol(format!("invalid hello: {error}")))?;
    if hello.message_type != "hello" || hello.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::Protocol(
            "unsupported hello protocol".to_owned(),
        ));
    }
    if hello.worker_version.is_empty() || hello.worker_version.len() > 64 {
        return Err(WorkerError::Protocol("invalid worker version".to_owned()));
    }
    if !hello
        .capabilities
        .iter()
        .any(|capability| capability == "extract")
        || hello
            .capabilities
            .iter()
            .any(|capability| capability != "extract")
    {
        return Err(WorkerError::Protocol(
            "unknown worker capability".to_owned(),
        ));
    }
    Ok(hello)
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

/// A single bounded browser-worker process.
#[derive(Clone)]
pub struct WorkerSupervisor {
    process: Arc<Mutex<Process>>,
    timeout: Duration,
    spool_root: PathBuf,
}

impl WorkerSupervisor {
    /// Spawn a worker command and validate its first JSONL frame as `hello`.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot start, the handshake times out, or the hello frame is invalid.
    pub async fn spawn(
        program: impl Into<PathBuf>,
        args: &[String],
        request_timeout: Duration,
    ) -> Result<Self, WorkerError> {
        let spool_root = std::env::var_os("WEBSIFT_SPOOL_ROOT")
            .map_or_else(|| PathBuf::from("/tmp/websift-spool"), PathBuf::from);
        Self::spawn_with_spool_root(program, args, request_timeout, spool_root).await
    }

    /// Spawn a worker with an explicit spool root owned by the caller.
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot start, the handshake times out, or the hello frame is invalid.
    pub async fn spawn_with_spool_root(
        program: impl Into<PathBuf>,
        args: &[String],
        request_timeout: Duration,
        spool_root: PathBuf,
    ) -> Result<Self, WorkerError> {
        let mut command = Command::new(program.into());
        command.kill_on_drop(true);
        command
            .args(args)
            .env("WEBSIFT_SPOOL_ROOT", &spool_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            // The worker is a console program. An MCP host is usually a GUI process with no
            // console to inherit, so Windows would open a visible console window for the worker
            // and steal focus from whatever the user is doing. Every stream is piped here, so the
            // window would carry no output anyway.
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(WorkerError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker stdin unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker stdout unavailable".to_owned()))?;
        let mut lines = BufReader::new(stdout).lines();
        let hello = timeout(request_timeout, lines.next_line())
            .await
            .map_err(|_| {
                WorkerError::Timeout(u64::try_from(request_timeout.as_millis()).unwrap_or(u64::MAX))
            })?
            .map_err(WorkerError::Io)?
            .ok_or(WorkerError::Eof)?;
        if hello.len() > MAX_FRAME_BYTES {
            return Err(WorkerError::Protocol("hello frame too large".to_owned()));
        }
        validate_hello_frame(&hello)?;
        Ok(Self {
            process: Arc::new(Mutex::new(Process {
                child,
                stdin,
                lines,
            })),
            timeout: request_timeout,
            spool_root,
        })
    }

    /// Send one request. Requests are serialized to preserve JSONL correlation.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol violations, worker failures, cancellation, I/O errors, or timeout.
    pub async fn request(&self, mut request: Request) -> Result<ResultMessage, WorkerError> {
        "request".clone_into(&mut request.message_type);
        request.protocol_version = PROTOCOL_VERSION;
        let mut process = self.process.lock().await;
        let frame = serde_json::to_string(&request)
            .map_err(|error| WorkerError::Protocol(error.to_string()))?;
        if frame.len() > MAX_FRAME_BYTES {
            return Err(WorkerError::Protocol("request frame too large".to_owned()));
        }
        process
            .stdin
            .write_all(frame.as_bytes())
            .await
            .map_err(WorkerError::Io)?;
        process
            .stdin
            .write_all(b"\n")
            .await
            .map_err(WorkerError::Io)?;
        process.stdin.flush().await.map_err(WorkerError::Io)?;
        let request_timeout = self.timeout.min(Duration::from_millis(request.deadline_ms));
        let deadline = Instant::now() + request_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.send_cancel(&mut process, &request.request_id).await?;
                process.child.kill().await.map_err(WorkerError::Io)?;
                return Err(WorkerError::Timeout(
                    request_timeout.as_millis().try_into().unwrap_or(u64::MAX),
                ));
            }
            let line = if let Ok(result) = timeout(remaining, process.lines.next_line()).await {
                result.map_err(WorkerError::Io)?.ok_or(WorkerError::Eof)?
            } else {
                self.send_cancel(&mut process, &request.request_id).await?;
                process.child.kill().await.map_err(WorkerError::Io)?;
                return Err(WorkerError::Timeout(
                    u64::try_from(request_timeout.as_millis()).unwrap_or(u64::MAX),
                ));
            };
            if line.len() > MAX_FRAME_BYTES {
                return Err(WorkerError::Protocol("response frame too large".to_owned()));
            }
            match serde_json::from_str::<Response>(&line)
                .map_err(|error| WorkerError::Protocol(error.to_string()))?
            {
                Response::Heartbeat {
                    protocol_version,
                    request_id,
                } => {
                    if protocol_version != PROTOCOL_VERSION {
                        return Err(WorkerError::Protocol(
                            "heartbeat protocol mismatch".to_owned(),
                        ));
                    }
                    if request_id
                        .as_deref()
                        .is_some_and(|id| id != request.request_id)
                    {
                        return Err(WorkerError::Protocol(
                            "heartbeat request mismatch".to_owned(),
                        ));
                    }
                }
                Response::Result(result) => {
                    validate_response_ids(
                        result.protocol_version,
                        &result.request_id,
                        &request.request_id,
                    )?;
                    if result.status != "ok" {
                        return Err(WorkerError::Protocol("invalid result status".to_owned()));
                    }
                    validate_artifacts(&result.artifacts, &request.spool_id, &self.spool_root)?;
                    return Ok(result);
                }
                Response::Error(error) => {
                    validate_response_ids(
                        error.protocol_version,
                        &error.request_id,
                        &request.request_id,
                    )?;
                    if error.status != "error" {
                        return Err(WorkerError::Protocol("invalid error status".to_owned()));
                    }
                    return Err(WorkerError::Remote {
                        code: error.code,
                        message: error.message,
                        retryable: error.retryable,
                    });
                }
            }
        }
    }

    async fn send_cancel(
        &self,
        process: &mut Process,
        request_id: &str,
    ) -> Result<(), WorkerError> {
        let cancel = serde_json::json!({"type":"cancel","protocol_version":PROTOCOL_VERSION,"request_id":request_id});
        process
            .stdin
            .write_all(cancel.to_string().as_bytes())
            .await
            .map_err(WorkerError::Io)?;
        process
            .stdin
            .write_all(b"\n")
            .await
            .map_err(WorkerError::Io)?;
        process.stdin.flush().await.map_err(WorkerError::Io)
    }
}

fn validate_artifacts(
    artifacts: &[Artifact],
    spool_id: &str,
    root: &Path,
) -> Result<(), WorkerError> {
    if artifacts.len() > MAX_ARTIFACTS {
        return Err(WorkerError::Protocol("too many artifacts".to_owned()));
    }
    for artifact in artifacts {
        if artifact.path.is_empty()
            || artifact.path.len() > 1024
            || Path::new(&artifact.path).is_absolute()
            || artifact
                .path
                .split('/')
                .any(|part| part == ".." || part.is_empty())
        {
            return Err(WorkerError::Protocol("invalid artifact path".to_owned()));
        }
        if artifact.bytes > MAX_ARTIFACT_BYTES
            || !artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || artifact.sha256.len() != 64
        {
            return Err(WorkerError::Protocol(
                "invalid artifact metadata".to_owned(),
            ));
        }
        let path = root.join(spool_id).join(&artifact.path);
        let bytes = fs::read(&path).map_err(WorkerError::Io)?;
        if bytes.len() as u64 != artifact.bytes
            || Sha256::digest(&bytes).to_vec() != hex_decode(&artifact.sha256)?
        {
            return Err(WorkerError::Protocol(
                "artifact integrity check failed".to_owned(),
            ));
        }
    }
    Ok(())
}

fn hex_decode(value: &str) -> Result<Vec<u8>, WorkerError> {
    (0..value.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&value[i..i + 2], 16)
                .map_err(|_| WorkerError::Protocol("invalid artifact hash".to_owned()))
        })
        .collect()
}

fn validate_response_ids(
    protocol_version: u8,
    response_id: &str,
    request_id: &str,
) -> Result<(), WorkerError> {
    if protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::Protocol(
            "response protocol mismatch".to_owned(),
        ));
    }
    if response_id != request_id {
        return Err(WorkerError::Protocol(
            "response request mismatch".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_hello_and_rejects_unknown_capabilities() {
        let frame = r#"{"type":"hello","protocol_version":1,"worker_version":"test","capabilities":["extract"]}"#;
        assert_eq!(validate_hello_frame(frame).unwrap().worker_version, "test");
        let bad = frame.replace("extract", "unknown");
        assert!(validate_hello_frame(&bad).is_err());
    }

    #[test]
    fn rejects_malformed_hello() {
        assert!(validate_hello_frame("not-json").is_err());
        assert!(
            validate_hello_frame(
                r#"{"type":"hello","protocol_version":2,"worker_version":"test","capabilities":[]}"#
            )
            .is_err()
        );
    }
}
