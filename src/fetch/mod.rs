//! Bounded native HTTP retrieval.

pub mod extract;
pub mod search;

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header::CONTENT_TYPE};

use crate::{
    config::Config,
    policy::{DnsResolver, PublicUrl, SystemDnsResolver, ValidatingDnsResolver},
};
use std::sync::Arc;

/// Successful bounded HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Transport-independent fetch failure.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("URL validation failed: {0:?}")]
    InvalidUrl(crate::policy::UrlError),
    #[error("HTTP request timed out: {0}")]
    Timeout(String),
    #[error("HTTP request failed: {0}")]
    Transport(String),
    #[error("response status was {0}")]
    Status(StatusCode),
    #[error("response did not include a content type")]
    MissingContentType,
    #[error("response content type is not allowed: {0}")]
    InvalidContentType(String),
    #[error("response exceeded maximum size of {limit} bytes")]
    BodyTooLarge { limit: u64 },
    #[error("response body could not be read: {0}")]
    ReadBody(#[source] std::io::Error),
}

/// Native HTTP client with explicit timeout and response-size bounds.
#[derive(Debug, Clone)]
pub struct FetchClient {
    client: Client,
    timeout: Duration,
    max_bytes: u64,
}

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ADDRESSES: usize = 16;

#[allow(clippy::needless_pass_by_value)]
fn fetch_transport_error(error: reqwest::Error) -> FetchError {
    let message = error.to_string();
    if error.is_timeout() {
        FetchError::Timeout(message)
    } else {
        FetchError::Transport(message)
    }
}

impl FetchClient {
    /// Build a client from process configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn from_config(config: &Config) -> Result<Self, FetchError> {
        Self::new(Duration::from_millis(config.timeout_ms), config.max_bytes)
    }

    /// Build a client with a request timeout and maximum response size.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(timeout: Duration, max_bytes: u64) -> Result<Self, FetchError> {
        Self::with_resolver(timeout, max_bytes, Arc::new(SystemDnsResolver))
    }

    /// Build a client with an injectable DNS resolver for tests and controlled runtimes.
    ///
    /// # Errors
    ///
    /// Returns an error if the response bound is zero or the HTTP client cannot be built.
    pub fn with_resolver(
        timeout: Duration,
        max_bytes: u64,
        resolver: Arc<dyn DnsResolver>,
    ) -> Result<Self, FetchError> {
        if max_bytes == 0 {
            return Err(FetchError::BodyTooLarge { limit: 0 });
        }
        let resolver =
            ValidatingDnsResolver::new(resolver, RESOLVE_TIMEOUT.min(timeout), MAX_ADDRESSES);
        let client = Client::builder()
            .dns_resolver(Arc::new(resolver))
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(fetch_transport_error)?;
        Ok(Self {
            client,
            timeout,
            max_bytes,
        })
    }

    /// Build a client whose DNS is overridden, for loopback tests only.
    ///
    /// This is the one seam that lets tests exercise real HTTP without relaxing URL policy, and
    /// it exists only in test builds.
    #[cfg(test)]
    pub(crate) fn with_dns_override<R: reqwest::dns::Resolve + 'static>(
        timeout: Duration,
        max_bytes: u64,
        resolver: Arc<R>,
    ) -> Result<Self, FetchError> {
        let client = Client::builder()
            .dns_resolver(resolver)
            .connect_timeout(timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(fetch_transport_error)?;
        Ok(Self {
            client,
            timeout,
            max_bytes,
        })
    }

    /// Fetch a public HTTP(S) URL using GET.
    ///
    /// The URL is validated before any network operation. The body is streamed and
    /// never buffered beyond `max_bytes`; responses with a declared larger body are
    /// rejected before reading.
    ///
    /// # Errors
    ///
    /// Returns validation, transport, status, content-type, or size-limit failures.
    pub async fn get(&self, input: &str) -> Result<FetchResult, FetchError> {
        let url = PublicUrl::parse(input).map_err(FetchError::InvalidUrl)?;
        let response = self
            .client
            .get(url.as_str())
            .timeout(self.timeout)
            .send()
            .await
            .map_err(fetch_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError::Status(status));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .ok_or(FetchError::MissingContentType)?
            .to_str()
            .map_err(|_| FetchError::InvalidContentType("invalid header value".to_owned()))?;
        let media_type = content_type
            .split(';')
            .next()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .filter(|value| {
                matches!(
                    value.as_str(),
                    "text/plain"
                        | "text/html"
                        | "application/xhtml+xml"
                        | "application/xml"
                        | "text/xml"
                )
            })
            .ok_or_else(|| FetchError::InvalidContentType(content_type.to_owned()))?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_bytes)
        {
            return Err(FetchError::BodyTooLarge {
                limit: self.max_bytes,
            });
        }

        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(fetch_transport_error)?;
            let new_len = body
                .len()
                .checked_add(chunk.len())
                .ok_or(FetchError::BodyTooLarge {
                    limit: self.max_bytes,
                })?;
            if usize::try_from(self.max_bytes).map_or(true, |limit| new_len > limit) {
                return Err(FetchError::BodyTooLarge {
                    limit: self.max_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(FetchResult {
            url: url.as_str().to_owned(),
            status: status.as_u16(),
            content_type: media_type,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FetchClient, FetchError};
    use std::{net::SocketAddr, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[allow(dead_code)]
    async fn server(response: &'static str) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 512];
            let _ = stream.read(&mut request).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        address
    }

    #[tokio::test]
    async fn rejects_private_destination_before_network_access() {
        let client = FetchClient::new(Duration::from_secs(1), 5).unwrap();
        let result = client.get("http://127.0.0.1/").await;
        assert!(matches!(result, Err(FetchError::InvalidUrl(_))));
    }

    #[test]
    fn rejects_zero_byte_bound() {
        assert!(matches!(
            FetchClient::new(Duration::from_secs(1), 0),
            Err(FetchError::BodyTooLarge { limit: 0 })
        ));
    }
}
