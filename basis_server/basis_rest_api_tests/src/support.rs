//! A deliberately tiny HTTP/1.1 client: one request per connection, no keep-alive, no chunked
//! request bodies. Enough to drive axum over loopback without pulling an HTTP client crate into
//! the workspace for the tests alone.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// One HTTP response: status code, headers as sent, body as text.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.body))
    }
}

pub struct HttpClient {
    addr: SocketAddr,
    headers: Vec<(String, String)>,
}

impl HttpClient {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr, headers: Vec::new() }
    }

    /// A client that sends `Authorization: Bearer <key>` on every request.
    pub fn with_bearer(addr: SocketAddr, key: &str) -> Self {
        let mut client = Self::new(addr);
        client.headers.push(("Authorization".to_string(), format!("Bearer {key}")));
        client
    }

    pub fn get(&self, path: &str) -> HttpResponse {
        self.request("GET", path, None)
    }

    pub fn delete(&self, path: &str) -> HttpResponse {
        self.request("DELETE", path, None)
    }

    pub fn post_json(&self, path: &str, json: &str) -> HttpResponse {
        self.request("POST", path, Some(json))
    }

    pub fn request(&self, method: &str, path: &str, body: Option<&str>) -> HttpResponse {
        let mut stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(5)).unwrap_or_else(|e| panic!("connect to {}: {e}", self.addr));
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();

        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n", self.addr);
        for (name, value) in &self.headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if let Some(body) = body {
            request.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
        } else if method != "GET" {
            request.push_str("Content-Length: 0\r\n");
        }
        request.push_str("\r\n");
        if let Some(body) = body {
            request.push_str(body);
        }
        stream.write_all(request.as_bytes()).unwrap_or_else(|e| panic!("write request: {e}"));

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap_or_else(|e| panic!("read response: {e}"));
        Self::parse(&raw)
    }

    fn parse(raw: &[u8]) -> HttpResponse {
        let text = String::from_utf8_lossy(raw);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status_line = lines.next().unwrap_or("");
        let status = status_line.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or_else(|| panic!("bad status line: {status_line:?}"));
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))).collect();
        let body = if headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked")) { Self::dechunk(body) } else { body.to_string() };
        HttpResponse { status, headers, body }
    }

    fn dechunk(body: &str) -> String {
        let mut out = String::new();
        let mut rest = body;
        while let Some((size_line, after)) = rest.split_once("\r\n") {
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            out.push_str(&after[..size.min(after.len())]);
            rest = after.get(size + 2..).unwrap_or("");
        }
        out
    }
}
