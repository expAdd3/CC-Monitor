//! Provider-independent notification delivery plus the ntfy JSON client.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::{header, redirect, Client, StatusCode};
use serde::Serialize;
use std::time::Duration;
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub priority: Priority,
    pub tag: String,
    pub session_id: Option<String>,
    pub client_bundle_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Priority {
    Min = 1,
    Low = 2,
    Default = 3,
    High = 4,
    Urgent = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NtfyConfig {
    pub server: String,
    pub topic: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{0}")]
    InvalidConfig(&'static str),
    #[error("{0}")]
    Delivery(&'static str),
}

impl NotifyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(code) | Self::Delivery(code) => code,
        }
    }
}

#[async_trait]
pub trait NotificationProvider: Send + Sync {
    async fn send(&self, notification: &Notification) -> Result<(), NotifyError>;
}

/// Adapter used to connect the engine to Tauri's notification
/// plugin without making the core depend on Tauri.
#[async_trait]
pub trait DesktopTransport: Send + Sync {
    async fn show(&self, notification: &Notification) -> Result<(), String>;
}

pub struct DesktopProvider<T> {
    transport: T,
}

impl<T> DesktopProvider<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl<T: DesktopTransport> NotificationProvider for DesktopProvider<T> {
    async fn send(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.transport
            .show(notification)
            .await
            .map_err(|_| NotifyError::Delivery("delivery_failed"))
    }
}

#[derive(Clone)]
pub struct NtfyProvider {
    client: Client,
    config: NtfyConfig,
    endpoint: Url,
}

impl NtfyProvider {
    pub fn new(config: NtfyConfig) -> Result<Self, NotifyError> {
        let endpoint = validate_server_url(&config.server)?;
        validate_topic(&config.topic)?;
        if endpoint.scheme() == "http"
            && (!config.username.is_empty() || !config.password.is_empty())
        {
            return Err(NotifyError::InvalidConfig(
                "ntfy_credentials_https_required",
            ));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            // A redirect could move a loopback HTTP request, including its
            // notification body, onto an untrusted plaintext endpoint.
            .redirect(redirect::Policy::none())
            .build()
            .map_err(|_| NotifyError::InvalidConfig("ntfy_client_init_failed"))?;
        Ok(Self {
            client,
            config,
            endpoint,
        })
    }
}

#[derive(Serialize)]
struct NtfyPayload<'a> {
    topic: &'a str,
    title: &'a str,
    message: &'a str,
    priority: u8,
    tags: [&'a str; 1],
}

#[async_trait]
impl NotificationProvider for NtfyProvider {
    async fn send(&self, notification: &Notification) -> Result<(), NotifyError> {
        let payload = NtfyPayload {
            topic: self.config.topic.trim(),
            title: &notification.title,
            message: &notification.body,
            priority: notification.priority as u8,
            tags: [&notification.tag],
        };
        let mut request = self.client.post(self.endpoint.clone()).json(&payload);
        if !self.config.username.is_empty() || !self.config.password.is_empty() {
            let token =
                STANDARD.encode(format!("{}:{}", self.config.username, self.config.password));
            request = request.header(header::AUTHORIZATION, format!("Basic {token}"));
        }
        let response = request
            .send()
            .await
            .map_err(|error| NotifyError::Delivery(network_error_code(&error)))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // Never read response content. A hostile provider may reflect
        // credentials or Authorization values in its body or HTML title.
        Err(NotifyError::Delivery(http_error_code(status)))
    }
}

pub fn validate_server_url(server: &str) -> Result<Url, NotifyError> {
    let trimmed = server.trim().trim_end_matches('/');
    let url =
        Url::parse(trimmed).map_err(|_| NotifyError::InvalidConfig("ntfy_server_url_invalid"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NotifyError::InvalidConfig("ntfy_server_http_required"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NotifyError::InvalidConfig(
            "ntfy_server_credentials_forbidden",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(NotifyError::InvalidConfig("ntfy_server_suffix_forbidden"));
    }
    if url.scheme() == "http" && !is_explicit_loopback_source(trimmed) {
        return Err(NotifyError::InvalidConfig("ntfy_server_https_required"));
    }
    Ok(url)
}

fn is_explicit_loopback_source(server: &str) -> bool {
    let Some((_, remainder)) = server.split_once("://") else {
        return false;
    };
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((host, _)) = ipv6.split_once(']') else {
            return false;
        };
        host
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _port)| host)
    };
    host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1")
}

/// Validates the topic grammar accepted by ntfy: 1-64 ASCII letters, digits,
/// hyphens, or underscores.
pub fn validate_topic(topic: &str) -> Result<(), NotifyError> {
    if topic.is_empty()
        || topic.len() > 64
        || !topic
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(NotifyError::InvalidConfig("ntfy_topic_invalid"));
    }
    Ok(())
}

fn http_error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => "ntfy_auth_failed",
        StatusCode::FORBIDDEN => "ntfy_permission_denied",
        StatusCode::NOT_FOUND => "ntfy_not_found",
        StatusCode::TOO_MANY_REQUESTS => "ntfy_rate_limited",
        status if status.is_server_error() => "ntfy_server_failed",
        _ => "ntfy_http_failed",
    }
}

fn network_error_code(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "ntfy_network_timeout"
    } else if error.is_connect() {
        "ntfy_network_connect"
    } else if error
        .to_string()
        .to_ascii_lowercase()
        .split_whitespace()
        .any(|part| {
            part.contains("tls") || part.contains("certificate") || part.contains("handshake")
        })
    {
        "ntfy_network_tls"
    } else {
        "ntfy_network_request"
    }
}

/// Maps adapter/provider failures to a bounded, non-sensitive diagnostic code.
///
/// Callers may persist or log this value. They must not persist or log the
/// original provider error because it can contain URLs, credentials, headers,
/// or response bodies.
pub fn diagnostic_code(value: &str) -> &'static str {
    let lower = value.to_ascii_lowercase();
    if let Some(code) = [
        "sensitive_error_redacted",
        "network_timeout",
        "network_unreachable",
        "ntfy_auth_failed",
        "ntfy_permission_denied",
        "ntfy_not_found",
        "ntfy_rate_limited",
        "ntfy_server_failed",
        "ntfy_http_failed",
        "ntfy_network_timeout",
        "ntfy_network_connect",
        "ntfy_network_tls",
        "ntfy_network_request",
        "http_client_error",
        "http_server_error",
        "invalid_configuration",
        "storage_failed",
        "permission_denied",
        "invalid_data",
        "io_failed",
        "delivery_failed",
    ]
    .into_iter()
    .find(|code| *code == lower)
    {
        return code;
    }
    if lower.contains("authorization")
        || lower.contains("credential")
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("secret")
    {
        "sensitive_error_redacted"
    } else if lower.contains("timeout") || lower.contains('超') && lower.contains('时') {
        "network_timeout"
    } else if lower.contains("connect")
        || lower.contains("dns")
        || lower.contains("network")
        || lower.contains("连接")
    {
        "network_unreachable"
    } else if lower.contains("http 4") {
        "http_client_error"
    } else if lower.contains("http 5") {
        "http_server_error"
    } else if lower.contains("config")
        || lower.contains("invalid")
        || lower.contains("无效")
        || lower.contains("必须")
    {
        "invalid_configuration"
    } else if lower.contains("database") || lower.contains("sqlite") {
        "storage_failed"
    } else if lower.contains("permission") || lower.contains("denied") {
        "permission_denied"
    } else if lower.contains("parse") || lower.contains("json") {
        "invalid_data"
    } else if lower.contains("i/o") || lower.contains("io error") {
        "io_failed"
    } else {
        "delivery_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc::{self, Receiver, RecvTimeoutError},
        time::{Duration as StdDuration, Instant},
    };

    fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
        stream.set_read_timeout(Some(StdDuration::from_secs(2)))?;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2_048];
        loop {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();
            if request.len() >= header_end.saturating_add(content_length) {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&request).into_owned())
    }

    fn accept_before(
        listener: &TcpListener,
        stop: &Receiver<()>,
        deadline: Instant,
    ) -> std::io::Result<Option<TcpStream>> {
        listener.set_nonblocking(true)?;
        loop {
            match listener.accept() {
                Ok((stream, _)) => return Ok(Some(stream)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            let wait = remaining.min(StdDuration::from_millis(20));
            match stop.recv_timeout(wait) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(None),
                Err(RecvTimeoutError::Timeout) if wait == remaining => return Ok(None),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    #[test]
    fn validates_ntfy_server_without_credentials_query_or_fragment() {
        assert!(validate_server_url("https://ntfy.example.com/").is_ok());
        for invalid in [
            "ntfy.example.com",
            "ftp://ntfy.example.com",
            "https://u:p@ntfy.example.com",
            "https://ntfy.example.com?q=secret",
            "https://ntfy.example.com/#secret",
        ] {
            assert!(validate_server_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn plaintext_ntfy_is_limited_to_explicit_loopback_without_credentials() {
        for server in [
            "http://localhost:8088",
            "http://127.0.0.1:8088",
            "http://[::1]:8088",
        ] {
            assert!(
                NtfyProvider::new(NtfyConfig {
                    server: server.into(),
                    topic: "monitor".into(),
                    username: String::new(),
                    password: String::new(),
                })
                .is_ok(),
                "credential-free loopback should be accepted: {server}"
            );
        }

        for server in [
            "http://ntfy.example.com",
            "http://localhost.example.com",
            "http://localhost.",
            "http://127.0.0.2",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://0.0.0.0",
            "http://[::ffff:127.0.0.1]",
        ] {
            let error = NtfyProvider::new(NtfyConfig {
                server: server.into(),
                topic: "monitor".into(),
                username: String::new(),
                password: String::new(),
            })
            .err()
            .expect("plaintext non-loopback must be rejected");
            assert_eq!(error.code(), "ntfy_server_https_required", "{server}");
        }
    }

    #[test]
    fn plaintext_loopback_rejects_every_credential_combination() {
        for (username, password) in [("monitor", ""), ("", "secret"), ("monitor", "secret")] {
            let error = NtfyProvider::new(NtfyConfig {
                server: "http://127.0.0.1:8088".into(),
                topic: "monitor".into(),
                username: username.into(),
                password: password.into(),
            })
            .err()
            .expect("credentials require TLS even on loopback");
            assert_eq!(error.code(), "ntfy_credentials_https_required");
        }

        assert!(NtfyProvider::new(NtfyConfig {
            server: "https://ntfy.example.com".into(),
            topic: "monitor".into(),
            username: "monitor".into(),
            password: "secret".into(),
        })
        .is_ok());
    }

    #[tokio::test]
    async fn redirect_target_never_receives_notification_content_or_authorization() {
        let deadline = Instant::now() + StdDuration::from_secs(10);
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_address = target.local_addr().unwrap();
        let (target_stop, target_stopped) = mpsc::channel();
        let target_server = std::thread::spawn(move || -> std::io::Result<Option<String>> {
            let Some(mut stream) = accept_before(&target, &target_stopped, deadline)? else {
                return Ok(None);
            };
            let request = read_http_request(&mut stream)?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            Ok(Some(request))
        });

        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let source_address = source.local_addr().unwrap();
        let (source_stop, source_stopped) = mpsc::channel();
        let source_server = std::thread::spawn(move || -> std::io::Result<Option<String>> {
            let Some(mut stream) = accept_before(&source, &source_stopped, deadline)? else {
                return Ok(None);
            };
            let request = read_http_request(&mut stream)?;
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes())?;
            Ok(Some(request))
        });

        let provider = NtfyProvider::new(NtfyConfig {
            server: format!("http://{source_address}"),
            topic: "monitor".into(),
            username: String::new(),
            password: String::new(),
        })
        .unwrap();
        let result = provider
            .send(&Notification {
                title: "redirect-sensitive-title".into(),
                body: "redirect-sensitive-body".into(),
                priority: Priority::Default,
                tag: "test".into(),
                session_id: None,
                client_bundle_id: None,
            })
            .await;

        let _ = source_stop.send(());
        let _ = target_stop.send(());
        let source_request = source_server
            .join()
            .unwrap()
            .unwrap()
            .expect("source server did not receive the original request");
        assert!(source_request.contains("redirect-sensitive-body"));
        assert_eq!(
            result.unwrap_err().code(),
            "ntfy_http_failed",
            "the original 307 response must be surfaced instead of followed"
        );
        assert!(
            target_server.join().unwrap().unwrap().is_none(),
            "redirect target unexpectedly received an HTTP request"
        );
    }

    #[test]
    fn topic_contract_matches_shared_fixtures() {
        #[derive(serde::Deserialize)]
        struct TopicFixtures {
            valid: Vec<String>,
            invalid: Vec<String>,
        }

        let fixtures: TopicFixtures =
            serde_json::from_str(include_str!("../../../tests/fixtures/ntfy_topics.json")).unwrap();
        for topic in fixtures.valid {
            assert!(validate_topic(&topic).is_ok(), "valid topic: {topic:?}");
        }
        for topic in fixtures.invalid {
            assert!(validate_topic(&topic).is_err(), "invalid topic: {topic:?}");
        }
    }

    #[test]
    fn classifies_http_status_without_accepting_response_content() {
        assert_eq!(
            http_error_code(StatusCode::UNAUTHORIZED),
            "ntfy_auth_failed"
        );
        assert_eq!(
            http_error_code(StatusCode::FORBIDDEN),
            "ntfy_permission_denied"
        );
        assert_eq!(http_error_code(StatusCode::NOT_FOUND), "ntfy_not_found");
        assert_eq!(
            http_error_code(StatusCode::TOO_MANY_REQUESTS),
            "ntfy_rate_limited"
        );
        assert_eq!(
            http_error_code(StatusCode::BAD_GATEWAY),
            "ntfy_server_failed"
        );
        assert_eq!(http_error_code(StatusCode::IM_A_TEAPOT), "ntfy_http_failed");
    }

    #[test]
    fn hostile_response_content_cannot_influence_status_only_classification() {
        let hostile = "Authorization: Basic reflected-password <title>secret</title>";
        let error = NotifyError::Delivery(http_error_code(StatusCode::FORBIDDEN));
        assert_eq!(error.code(), "ntfy_permission_denied");
        assert_eq!(error.to_string(), "ntfy_permission_denied");
        for secret in ["Authorization", "password", "secret", hostile] {
            assert!(!error.to_string().contains(secret));
        }
    }

    #[test]
    fn diagnostic_codes_are_fixed_and_never_echo_sensitive_input() {
        assert_eq!(
            diagnostic_code("Authorization: Basic secret"),
            "sensitive_error_redacted"
        );
        assert_eq!(diagnostic_code("request timeout"), "network_timeout");
        assert_eq!(
            diagnostic_code("ntfy returned HTTP 503"),
            "http_server_error"
        );
        assert_eq!(
            diagnostic_code("/private/transcript.jsonl exploded"),
            "invalid_data"
        );
    }
}
