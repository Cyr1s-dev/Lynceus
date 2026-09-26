//! 多 Provider LLM 网关 —— `server/engines/model_providers/` 的移植。
//!
//! Python 侧单文件 `runtime.py` 按职责拆为子模块，公开面镜像其
//! `__init__.py` 导出（运行时/路由/密钥 store）；错误类型、payload
//! 构建与 SSE 解析为 Rust 侧的结构化承接。`mock` 与 `sse` 是 M4
//! 明确要求的 Rust 侧新增（测试不触网 / 流式能力），Python 无对应物。
//!
//! 路由选择（按 purpose 查路由表）留在 engine/app 层，本模块只负责
//! 逐条 fallback 与熔断计数——与 Python 的依赖边界一致。

mod error;
mod mock;
mod payload;
mod redact;
mod router;
mod runtime;
mod secrets;
mod sse;
mod stream;
mod urls;

pub use error::GatewayError;
pub use error::format_model_discovery_error;
pub use error::format_provider_error;
pub use error::provider_error_status;
pub use error::response_error_detail;
pub use mock::MockProviderRuntime;
pub use mock::RecordedRequest;
pub use payload::build_payload;
pub use payload::extract_discovered_model_ids;
pub use payload::extract_model;
pub use payload::extract_provider_text;
pub use payload::extract_usage;
pub use payload::parse_structured_response;
pub use redact::hash_text;
pub use redact::python_json_dumps_messages;
pub use redact::redact_secrets;
pub use redact::summarize_messages;
pub use redact::summarize_text;
pub use router::ProviderRouterRuntime;
pub use runtime::OpenAiCompatibleProviderRuntime;
pub use secrets::EnvironmentSecretStore;
pub use secrets::PlaintextSecretStore;
pub use secrets::SecretStore;
pub use sse::SseEvent;
pub use sse::SseParser;
pub use sse::extract_stream_delta;
pub use sse::extract_stream_model;
pub use sse::extract_stream_usage;
pub use stream::GatewayStreamItem;
pub use stream::StreamOutcome;
pub use urls::normalize_base_url;
