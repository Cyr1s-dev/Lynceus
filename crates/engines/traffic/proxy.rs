//! MITM 代理服务器：CONNECT 解密 + 请求转发 + 交换录制。
//!
//! 落地面（对齐参考实现的行为面，按 Rust 可行性裁剪）：
//! - **显式正向代理**（非透明）：worker 经 `HTTP(S)_PROXY` 环境变量流入；
//! - **CONNECT MITM**：TCP 层 peek 识别 CONNECT（绕开 hyper upgrade API——
//!   hyper 1.x 的 `Upgraded` 只实现自家 `rt::Read/Write`，桥接到 tokio 生态
//!   代价不对等），按 host 签叶证书（[`CertAuthority`]）解密 TLS，上游用
//!   真实证书校验转发（`reqwest` + rustls）；
//! - **plain HTTP**（absolute-URI）直接转发录制；
//! - 每条完成的交换落 [`ExchangeStore`]；
//! - **不做 h2 / websocket**：ALPN 只广告 http/1.1；
//! - **LLM 流量不进来**：由注入侧的 `NO_PROXY=127.0.0.1` 保证。

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::Request;
use hyper::Response;
use hyper::body::Bytes;
use hyper::body::Incoming;
use hyper::service::Service;
use hyper_util::rt::TokioIo;
use std::pin::Pin;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use super::ca::CertAuthority;
use super::store::{ExchangeRecord, ExchangeStore};

/// 单请求体大小上限（防内存放大；超出截断仍录制）。
const MAX_BODY: usize = 10 * 1024 * 1024;
/// 单次转发超时。
const FORWARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

type BoxBody = Full<Bytes>;

/// 启动代理，返回 worker 该用的 `http://host:port` 地址。
///
/// # Errors
/// 地址解析 / 监听失败。
pub async fn spawn_proxy(
    addr: &str,
    ca: Arc<CertAuthority>,
    store: Arc<ExchangeStore>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> Result<String, String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| format!("bind {addr}: {error}"))?;
    let local = listener
        .local_addr()
        .map_err(|error| format!("local addr: {error}"))?;
    let proxy_addr = format!("http://{local}");

    let forwarder = Arc::new(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(FORWARD_TIMEOUT)
            .build()
            .map_err(|error| format!("build forward client: {error}"))?,
    );
    let seq = Arc::new(AtomicU64::new(0));

    tokio::spawn(async move {
        let mut shutdown = shutdown;
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = &mut shutdown => break,
            };
            let Ok((stream, _peer)) = accepted else {
                continue;
            };
            let ca = Arc::clone(&ca);
            let store = Arc::clone(&store);
            let forwarder = Arc::clone(&forwarder);
            let seq = Arc::clone(&seq);
            tokio::spawn(async move {
                if let Err(error) = serve_connection(stream, ca, store, forwarder, seq).await {
                    tracing::debug!(error = %error, "proxy connection ended");
                }
            });
        }
    });

    Ok(proxy_addr)
}

/// 单连接入口：peek 区分 CONNECT 隧道与明文代理请求。
async fn serve_connection(
    mut stream: tokio::net::TcpStream,
    ca: Arc<CertAuthority>,
    store: Arc<ExchangeStore>,
    forwarder: Arc<reqwest::Client>,
    seq: Arc<AtomicU64>,
) -> Result<(), String> {
    let mut peek_buf = [0u8; 16];
    let peeked = stream
        .peek(&mut peek_buf)
        .await
        .map_err(|error| format!("peek: {error}"))?;
    if peeked == 0 {
        return Ok(());
    }

    if peek_buf.starts_with(b"CONNECT ") {
        // 读掉 CONNECT 头（peek 不消费，正常读即可）。
        let mut header = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| format!("read connect header: {error}"))?;
            if read == 0 {
                return Err("connect header truncated".to_string());
            }
            header.extend_from_slice(&chunk[..read]);
            if header.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if header.len() > 8 * 1024 {
                return Err("connect header too large".to_string());
            }
        }
        let request_line = String::from_utf8_lossy(&header);
        let Some(authority) = request_line.split_whitespace().nth(1) else {
            return Err("malformed CONNECT line".to_string());
        };
        let host = authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
            .to_string();
        if host.is_empty() {
            return Err("empty CONNECT host".to_string());
        }
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|error| format!("write connect ack: {error}"))?;

        let server_config = ca.server_config_for(&host)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
        let tls_stream = acceptor
            .accept(stream)
            .await
            .map_err(|error| format!("tls accept {host}: {error}"))?;

        let service = TunnelService {
            host,
            store,
            forwarder,
            seq,
        };
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(tls_stream), service)
            .await
            .map_err(|error| format!("tunnel serve: {error}"))
    } else {
        let service = PlainService {
            store,
            forwarder,
            seq,
        };
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await
            .map_err(|error| format!("plain serve: {error}"))
    }
}

/// 明文面服务：absolute-URI 转发录制。
struct PlainService {
    store: Arc<ExchangeStore>,
    forwarder: Arc<reqwest::Client>,
    seq: Arc<AtomicU64>,
}

impl Service<Request<Incoming>> for PlainService {
    type Response = Response<BoxBody>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let store = Arc::clone(&self.store);
        let forwarder = Arc::clone(&self.forwarder);
        let seq = Arc::clone(&self.seq);
        Box::pin(async move {
            // 明文转发代理收到的是 absolute-URI 请求行（`http://host/path`），
            // 要转发的 URL 就是它本身。早先 `strip_prefix("http")` 会把 scheme
            // 削成 `://host/path`，reqwest 解析失败 → "builder error" → 502，
            // 于是所有明文 HTTP 目标都被代理打废（直连却正常）。这里原样使用
            // 完整 URI，只校验它确实是 http(s) absolute-URI。
            let url = req.uri().to_string();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Ok(text_response(400, "expected absolute-URI or CONNECT"));
            }
            let host = req.uri().host().unwrap_or_default().to_string();
            forward_and_record(&store, &forwarder, &seq, host, req, url).await
        })
    }
}

/// 解密隧道面服务：origin-form 请求按 `https://host` 转发录制。
struct TunnelService {
    host: String,
    store: Arc<ExchangeStore>,
    forwarder: Arc<reqwest::Client>,
    seq: Arc<AtomicU64>,
}

impl Service<Request<Incoming>> for TunnelService {
    type Response = Response<BoxBody>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let store = Arc::clone(&self.store);
        let forwarder = Arc::clone(&self.forwarder);
        let seq = Arc::clone(&self.seq);
        let host = self.host.clone();
        Box::pin(async move {
            if req.method() == hyper::Method::CONNECT {
                return Ok(text_response(400, "nested CONNECT is not supported"));
            }
            let path = req
                .uri()
                .path_and_query()
                .map_or("/", |value| value.as_str())
                .to_string();
            let url = format!("https://{host}{path}");
            forward_and_record(&store, &forwarder, &seq, host, req, url).await
        })
    }
}

/// 转发 + 录制的共享实现（两个服务面都用它）。
async fn forward_and_record(
    store: &Arc<ExchangeStore>,
    forwarder: &Arc<reqwest::Client>,
    seq: &Arc<AtomicU64>,
    host: String,
    req: Request<Incoming>,
    url: String,
) -> Result<Response<BoxBody>, hyper::Error> {
    let method = req.method().clone();
    let req_head = render_request_head(&method, &url, req.headers());
    let req_body = collect_body(req.into_body()).await;

    let mut builder = forwarder.request(method, &url);
    for (name, value) in &req_head.headers {
        if name != hyper::header::HOST && name != hyper::header::CONTENT_LENGTH {
            builder = builder.header(name, value);
        }
    }
    builder = builder.body(req_body.clone());

    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            // 目标侧错误不录制（没有完整交换）；如实回给客户端。
            tracing::debug!(error = %error, url = %url, "forward failed");
            return Ok(text_response(502, &format!("forward failed: {error}")));
        }
    };
    let status = response.status();
    let resp_head = render_response_head(&status, response.headers());
    let resp_body = match response.bytes().await {
        Ok(bytes) => bytes.to_vec(),
        Err(error) => {
            tracing::debug!(error = %error, url = %url, "response body read failed");
            Vec::new()
        }
    };

    let id = format!(
        "{}-{:04}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        seq.fetch_add(1, Ordering::Relaxed) % 10000
    );
    if let Err(error) = store.record(&ExchangeRecord {
        id,
        method: req_head.method.to_string(),
        url,
        host,
        status: status.as_u16(),
        req_head: req_head.text,
        req_body,
        resp_head: resp_head.text,
        resp_body: resp_body.clone(),
    }) {
        tracing::debug!(error = %error, "exchange record failed");
    }

    let mut response_builder = Response::builder().status(status);
    if let Some(headers) = response_builder.headers_mut() {
        for (name, value) in &resp_head.headers {
            if name != hyper::header::TRANSFER_ENCODING && name != hyper::header::CONTENT_LENGTH {
                headers.insert(name.clone(), value.clone());
            }
        }
    }
    Ok(response_builder
        .body(BoxBody::from(resp_body))
        .unwrap_or_else(|_| text_response(500, "response build failed")))
}

/// URL → (host, path)（可回放口径用）。
fn split_url(url: &str) -> (String, String) {
    let after = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = after.split('/').next().unwrap_or("").to_string();
    let path = after
        .get(host.len()..)
        .filter(|rest| !rest.is_empty())
        .map_or_else(|| "/".to_string(), str::to_string);
    (host, path)
}

/// 请求头原文渲染（请求行 + 头，恢复 Host）。
fn render_request_head(
    method: &hyper::Method,
    url: &str,
    headers: &hyper::HeaderMap,
) -> RenderedHead {
    let (host, path) = split_url(url);
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    let mut out = hyper::HeaderMap::new();
    for (name, value) in headers {
        if name == hyper::header::HOST {
            continue;
        }
        text.push_str(&format!(
            "{name}: {}\r\n",
            value.to_str().unwrap_or("<binary>")
        ));
        out.insert(name.clone(), value.clone());
    }
    RenderedHead {
        method: method.clone(),
        text,
        headers: out,
    }
}

/// 响应头原文渲染。
fn render_response_head(status: &hyper::StatusCode, headers: &hyper::HeaderMap) -> RenderedHead {
    let mut text = format!("HTTP/1.1 {}\r\n", status.as_u16());
    let mut out = hyper::HeaderMap::new();
    for (name, value) in headers {
        if name == hyper::header::TRANSFER_ENCODING {
            continue;
        }
        text.push_str(&format!(
            "{name}: {}\r\n",
            value.to_str().unwrap_or("<binary>")
        ));
        out.insert(name.clone(), value.clone());
    }
    RenderedHead {
        method: hyper::Method::GET,
        text,
        headers: out,
    }
}

struct RenderedHead {
    method: hyper::Method,
    text: String,
    headers: hyper::HeaderMap,
}

/// 有界收集请求体。
async fn collect_body(body: Incoming) -> Vec<u8> {
    match body.collect().await {
        Ok(collected) => {
            let bytes = collected.to_bytes();
            if bytes.len() > MAX_BODY {
                bytes[..MAX_BODY].to_vec()
            } else {
                bytes.to_vec()
            }
        }
        Err(_) => Vec::new(),
    }
}

/// 纯文本响应。
fn text_response(status: u16, message: &str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(BoxBody::from(message.as_bytes().to_vec()))
        .unwrap_or_else(|_| Response::new(BoxBody::from(Vec::new())))
}
