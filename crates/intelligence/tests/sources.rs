//! 真实 Intelligence source 的 hermetic HTTP 与 normalization 测试。

#![cfg_attr(test, allow(clippy::expect_used))]

use std::time::Duration;

use intelligence::{CrtShSource, IntelligenceSource, WaybackSource};
use models::{IntelEntityKind, IntelQuery, IntelQueryType, IntelRelationKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn mock_http(status: u16, body: &str, content_length: Option<usize>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock binds");
    let address = listener.local_addr().expect("mock address");
    let body = body.to_string();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = vec![0_u8; 8192];
        let _ = socket.read(&mut request).await;
        let length = content_length.unwrap_or(body.len());
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\ncontent-type: application/json\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n{body}"
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{address}")
}

async fn hanging_http() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock binds");
    let address = listener.local_addr().expect("mock address");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = vec![0_u8; 8192];
        let _ = socket.read(&mut request).await;
        std::future::pending::<()>().await;
    });
    format!("http://{address}")
}

#[tokio::test]
async fn crtsh_query_and_normalization_are_hermetic() {
    let base = mock_http(
        200,
        r#"[{"id":42,"name_value":"api.example.com\n*.Example.COM","common_name":"www.example.com","serial_number":"ABC123","issuer_name":"Example CA"}]"#,
        None,
    )
    .await;
    let source = CrtShSource::with_base_url(&base);
    let query = IntelQuery::new("example.com", IntelQueryType::Domain);
    let result = source.query(&query).await.expect("crt query succeeds");

    assert_eq!(result.record_count, 1);
    let normalized = source.normalize(&result.records[0]);
    assert!(normalized.entities.iter().any(|entity| {
        entity.kind == IntelEntityKind::Domain && entity.normalized_value == "api.example.com"
    }));
    assert!(normalized.relations.iter().any(|relation| {
        relation.relation == IntelRelationKind::CertificateContains
            && relation.to.1 == "api.example.com"
    }));
}

#[tokio::test]
async fn wayback_query_and_normalization_are_hermetic() {
    let base = mock_http(
        200,
        r#"[["urlkey","timestamp","original"],["com,example)/api","20200101000000","https://API.Example.com/v1#fragment"]]"#,
        None,
    )
    .await;
    let source = WaybackSource::with_base_url(&base);
    let query = IntelQuery::new("example.com", IntelQueryType::Domain);
    let result = source.query(&query).await.expect("wayback query succeeds");

    assert_eq!(result.record_count, 1);
    let normalized = source.normalize(&result.records[0]);
    assert!(normalized.entities.iter().any(|entity| {
        entity.kind == IntelEntityKind::Url
            && entity.normalized_value == "https://api.example.com/v1"
    }));
    assert_eq!(normalized.relations.len(), 1);
}

#[tokio::test]
async fn source_http_and_malformed_payload_errors_are_explicit() {
    let http_error = mock_http(503, r#"{"error":"unavailable"}"#, None).await;
    let source = CrtShSource::with_base_url(&http_error);
    let query = IntelQuery::new("example.com", IntelQueryType::Domain);
    let error = source.query(&query).await.expect_err("503 must fail");
    assert!(error.to_string().contains("HTTP 503"));

    let malformed = mock_http(200, r#"{"not":"cdx rows"}"#, None).await;
    let source = WaybackSource::with_base_url(&malformed);
    let error = source
        .query(&query)
        .await
        .expect_err("malformed payload must fail");
    assert!(error.to_string().contains("parse"));
}

#[tokio::test]
async fn source_timeout_and_payload_limit_are_enforced() {
    let hanging = hanging_http().await;
    let source = CrtShSource::with_base_url_and_timeout(&hanging, Duration::from_millis(20));
    let query = IntelQuery::new("example.com", IntelQueryType::Domain);
    let error = source.query(&query).await.expect_err("timeout must fail");
    assert!(error.to_string().contains("timeout"));

    let oversized = mock_http(200, "[]", Some(5 * 1024 * 1024)).await;
    let source = CrtShSource::with_base_url(&oversized);
    let error = source.query(&query).await.expect_err("oversize must fail");
    assert!(error.to_string().contains("exceeds"));
}
