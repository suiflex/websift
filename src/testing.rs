//! Test-only loopback HTTP harness.
//!
//! Production policy forbids private destinations and non-default ports, which is exactly what a
//! local test server needs. Rather than weakening that policy behind a runtime flag, this module
//! is compiled only under `cfg(test)`: it supplies a reqwest DNS override that maps test
//! hostnames onto a loopback listener, so the URL policy stays untouched in every shipped build.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// One canned HTTP response.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into(),
        }
    }
    pub fn html(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/html",
            body: body.into(),
        }
    }
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/plain",
            body: body.into(),
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: String::new(),
        }
    }

    fn render(&self) -> String {
        format!(
            "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status,
            self.content_type,
            self.body.len(),
            self.body
        )
    }
}

#[derive(Default)]
struct Routes {
    /// Scripted replies per path. The last reply repeats once the queue is down to one entry.
    replies: HashMap<String, Vec<Reply>>,
    hits: HashMap<String, usize>,
}

/// Loopback HTTP server that answers scripted replies and records what was requested.
pub struct TestServer {
    address: SocketAddr,
    routes: Arc<Mutex<Routes>>,
    peak_in_flight: Arc<AtomicUsize>,
}

impl TestServer {
    /// Start a server that pauses `delay` before each response, which makes concurrency
    /// observable.
    pub async fn start(delay: Duration) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("addr");
        let routes = Arc::new(Mutex::new(Routes::default()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak_in_flight = Arc::new(AtomicUsize::new(0));
        let server = Self {
            address,
            routes: Arc::clone(&routes),
            peak_in_flight: Arc::clone(&peak_in_flight),
        };
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let routes = Arc::clone(&routes);
                let in_flight = Arc::clone(&in_flight);
                let peak = Arc::clone(&peak_in_flight);
                tokio::spawn(async move {
                    let active = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(active, Ordering::SeqCst);
                    handle(stream, &routes, delay).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        server
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Queue replies for one path. Each request consumes one entry until the last one, which
    /// repeats, so a steady route needs a single reply.
    pub fn route(&self, path: &str, replies: Vec<Reply>) -> &Self {
        self.routes
            .lock()
            .expect("routes")
            .replies
            .insert(path.to_owned(), replies);
        self
    }

    pub fn hits(&self, path: &str) -> usize {
        self.routes
            .lock()
            .expect("routes")
            .hits
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    pub fn peak_concurrency(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }
}

async fn handle(mut stream: TcpStream, routes: &Arc<Mutex<Routes>>, delay: Duration) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 2048];
    // Read only the head; the tests never send a body.
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    }
    let head = String::from_utf8_lossy(&request);
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let path = target
        .split('?')
        .next()
        .unwrap_or(&target)
        .trim_end_matches('/')
        .to_owned();
    let path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };

    let reply = {
        let mut routes = routes.lock().expect("routes");
        *routes.hits.entry(path.clone()).or_insert(0) += 1;
        match routes.replies.get_mut(&path) {
            Some(queue) if queue.len() > 1 => queue.remove(0),
            Some(queue) => queue.first().cloned().unwrap_or_else(|| Reply::status(404)),
            None => Reply::status(404),
        }
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let _ = stream.write_all(reply.render().as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// reqwest resolver that maps test hostnames onto loopback listeners.
#[derive(Debug, Default)]
pub struct LoopbackResolver {
    hosts: HashMap<String, SocketAddr>,
}

impl LoopbackResolver {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn map(mut self, host: &str, address: SocketAddr) -> Self {
        self.hosts.insert(host.to_owned(), address);
        self
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl reqwest::dns::Resolve for LoopbackResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let address = self.hosts.get(name.as_str()).copied();
        Box::pin(async move {
            let address = address.ok_or_else(|| "unmapped test host".to_owned())?;
            let addrs: reqwest::dns::Addrs = Box::new(std::iter::once(address));
            Ok(addrs)
        })
    }
}
