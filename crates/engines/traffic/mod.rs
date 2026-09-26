//! 流量录制（traffic recording）：嵌入式 MITM 代理 + SQLite 交换存储。
//!
//! # 架构定位（P3，混合方案）
//!
//! 参考实现用 go-mitmproxy 全量录制 worker 的 HTTP 交互并绑定到 finding。
//! Lynceus 的 worker 是黑盒 CLI，唯一通用的捕获面是**子进程网络出口**：
//! 给 CLI 注入 `HTTP_PROXY`/`HTTPS_PROXY` + 自签 CA 信任变量，让它和它
//! spawn 的 curl/python/nuclei 全部流经本代理。
//!
//! ```text
//! worker CLI ──HTTPS_PROXY──▶ RecordingProxy ──TLS(真实证书)──▶ 目标
//!              (env 注入)         │
//!                                ├─ CONNECT → 按 host 签叶证书 → 解密 → 转发
//!                                └─ 记录 (request, response) → SQLite
//! ```
//!
//! # 红线
//! - **LLM 流量不录制**：注入 `NO_PROXY=127.0.0.1,localhost`，模型 API
//!   （LiteLLM 网关）直连，不进代理；
//! - **CA 与数据只落 `data/traffic/`**（`_ca/` + `_index/` + `_blobs/`），
//!   不污染项目根目录；
//! - 录制是 best-effort：代理不可用/失败绝不阻塞 mission（worker 照常
//!   执行，只是没有流量证据）；
//! - 明文全量录制（含 Cookie/Authorization）——与参考实现一致，靶场
//!   场景下可接受；生产目标需先评估。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

mod ca;
mod proxy;
mod store;

pub use ca::CertAuthority;
pub use store::{ExchangeRecord, ExchangeStore};

use ca::CertAuthority as Ca;
use proxy::spawn_proxy;
use store::ExchangeStore as Store;

/// 默认代理监听地址（回环）。
pub const DEFAULT_PROXY_ADDR: &str = "127.0.0.1:8899";

/// 流量录制器：CA + 存储 + 代理生命周期的持有者。
pub struct TrafficRecorder {
    /// worker 该用的代理地址（`http://` 前缀）。
    proxy_addr: String,
    /// CA 证书 PEM 路径（注入子进程信任）。
    ca_cert_path: PathBuf,
    store: Arc<Store>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TrafficRecorder {
    /// 在 `data/traffic/` 下装配并启动代理（幂等由调用方保证）。
    ///
    /// # Errors
    /// CA 生成/读取、存储打开或监听失败。
    pub async fn start(data_dir: &std::path::Path, addr: &str) -> Result<Self, String> {
        // 路径必须绝对化：CA 路径要注入 worker 子进程的 SSL_CERT_FILE，
        // 而子进程 cwd 是 mission 工作区——相对路径在它那里解析不到
        // （实测 codex 因此 Transport channel closed）。
        let root = std::path::absolute(data_dir)
            .map_err(|error| format!("absolute data dir: {error}"))?
            .join("traffic");
        let ca = Arc::new(Ca::open(&root.join("_ca"))?);
        let store = Arc::new(Store::open(&root.join("_index"))?);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let proxy_addr = spawn_proxy(addr, Arc::clone(&ca), Arc::clone(&store), shutdown_rx)
            .await
            .map_err(|error| format!("traffic proxy listen on {addr} failed: {error}"))?;
        Ok(Self {
            proxy_addr,
            ca_cert_path: ca.ca_cert_path(),
            store,
            shutdown: Some(shutdown_tx),
        })
    }

    /// worker 子进程该收到的代理地址（`http://127.0.0.1:port`）。
    #[must_use]
    pub fn proxy_addr(&self) -> &str {
        &self.proxy_addr
    }

    /// CA 证书路径（`SSL_CERT_FILE` 等变量用）。
    #[must_use]
    pub fn ca_cert_path(&self) -> &std::path::Path {
        &self.ca_cert_path
    }

    /// 存储句柄（查询/测试用）。
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// 注入 worker 子进程的代理环境变量（`NO_PROXY` 保证 LLM 流量直连）。
    #[must_use]
    pub fn proxy_env(&self) -> Vec<(String, String)> {
        // 兜底闸：CA 文件必须真实存在才注入信任变量。缺失/不可读 → 整体放弃
        // 注入（返回空），让 worker 退化为"无流量录制"，而不是把一个坏
        // SSL_CERT_FILE 塞进它——那会让 worker 的 TLS 栈连 LLM 网关都失败
        // （实测 codex MCP Transport channel closed / dsh Connection error，
        // 三者同根：worker cwd 是 mission 工作区，相对 CA 路径在它那里解析不到）。
        if !self.ca_cert_path.is_file() {
            tracing::warn!(
                path = %self.ca_cert_path.display(),
                "traffic CA missing; skipping proxy env injection (workers run without capture)"
            );
            return Vec::new();
        }
        let ca = self.ca_cert_path.to_string_lossy().into_owned();
        vec![
            ("HTTP_PROXY".to_string(), self.proxy_addr.clone()),
            ("HTTPS_PROXY".to_string(), self.proxy_addr.clone()),
            ("http_proxy".to_string(), self.proxy_addr.clone()),
            ("https_proxy".to_string(), self.proxy_addr.clone()),
            ("ALL_PROXY".to_string(), self.proxy_addr.clone()),
            ("all_proxy".to_string(), self.proxy_addr.clone()),
            // Node 24+ 内置 fetch/http 默认不读 *_PROXY。
            ("NODE_USE_ENV_PROXY".to_string(), "1".to_string()),
            // 各生态的 CA 信任变量（实测口径，与参考实现一致）。
            ("SSL_CERT_FILE".to_string(), ca.clone()),
            ("CURL_CA_BUNDLE".to_string(), ca.clone()),
            ("REQUESTS_CA_BUNDLE".to_string(), ca.clone()),
            ("GIT_SSL_CAINFO".to_string(), ca.clone()),
            ("NODE_EXTRA_CA_CERTS".to_string(), ca),
            // LLM 网关（127.0.0.1）永不进代理。
            (
                "NO_PROXY".to_string(),
                "127.0.0.1,localhost,::1".to_string(),
            ),
            (
                "no_proxy".to_string(),
                "127.0.0.1,localhost,::1".to_string(),
            ),
        ]
    }
}

impl Drop for TrafficRecorder {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

// --- 进程级单例（与 gateway / worker registry 同先例） ---------------------

static GLOBAL_TRAFFIC: OnceLock<Arc<TrafficRecorder>> = OnceLock::new();

/// 装配并登记进程级流量录制器。
///
/// 运行时内（生产路径：`async fn main` → `build_production_manager`）走
/// spawn——`block_on` 会在运行时内 panic；recorder 就绪前 worker 派发看
/// 不到代理（毫秒级窗口，无流量录制的 mission 照常执行）。运行时外
/// （测试）直接 block_on 同步装配。
pub fn attach_global_traffic(data_dir: &std::path::Path, addr: &str) {
    if GLOBAL_TRAFFIC.get().is_some() {
        return;
    }
    let data_dir = data_dir.to_path_buf();
    let addr = addr.to_string();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                match TrafficRecorder::start(&data_dir, &addr).await {
                    Ok(recorder) => {
                        let _ = GLOBAL_TRAFFIC.set(Arc::new(recorder));
                        tracing::info!(addr = %addr, "traffic recorder attached");
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "traffic recorder unavailable; workers run without capture");
                    }
                }
            });
        }
        Err(_) => {
            // 运行时外（测试等）：临时 runtime 同步装配。
            let Ok(runtime) = tokio::runtime::Runtime::new() else {
                tracing::warn!("traffic recorder unavailable: cannot build runtime");
                return;
            };
            match runtime.block_on(TrafficRecorder::start(&data_dir, &addr)) {
                Ok(recorder) => {
                    let _ = GLOBAL_TRAFFIC.set(Arc::new(recorder));
                }
                Err(error) => {
                    tracing::warn!(error = %error, "traffic recorder unavailable; workers run without capture");
                }
            }
        }
    }
}

/// 当前进程的流量录制器（未 attach 或启动失败为 None）。
#[must_use]
pub fn global_traffic() -> Option<Arc<TrafficRecorder>> {
    GLOBAL_TRAFFIC.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：起代理 → curl 经代理打真实 HTTPS → 交换必须落库。
    ///
    /// 打真实网络，默认不跑：`cargo test -p engines --lib traffic::tests::e2e -- --ignored`。
    #[tokio::test]
    #[ignore = "hits the public internet"]
    async fn e2e_proxy_records_https_exchange() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = TrafficRecorder::start(dir.path(), "127.0.0.1:0")
            .await
            .expect("recorder starts");
        let ca = recorder.ca_cert_path().to_string_lossy().into_owned();

        // curl 经代理 + 信任自签 CA。
        let output = tokio::process::Command::new("curl")
            .args([
                "-s",
                "-o",
                "NUL",
                "-w",
                "%{http_code}",
                "-x",
                recorder.proxy_addr(),
                "--cacert",
                &ca,
                "https://example.com/",
            ])
            .output()
            .await
            .expect("curl runs");
        assert!(
            output.status.success(),
            "curl through proxy must succeed; code={:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "200");

        let recent = recorder.store().recent(10).expect("recent");
        assert_eq!(recent.len(), 1, "exactly one exchange recorded: {recent:?}");
        assert_eq!(recent[0].1, "GET");
        assert_eq!(recent[0].2, "https://example.com/");
        assert_eq!(recent[0].3, 200);
    }

    /// 兜底闸：CA 文件缺失时 `proxy_env` 必须整体放弃注入，绝不吐出坏路径
    /// （否则 worker 的 TLS 栈连 127.0.0.1 网关都会失败）。
    #[tokio::test]
    async fn proxy_env_skips_injection_when_ca_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = TrafficRecorder::start(dir.path(), "127.0.0.1:0")
            .await
            .expect("recorder starts");
        // 健康时注入齐全（含 CA 信任变量）。
        assert!(
            recorder
                .proxy_env()
                .iter()
                .any(|(key, _)| key == "SSL_CERT_FILE"),
            "healthy recorder must inject the CA trust var"
        );
        // 删掉 CA 文件 → 整体放弃注入，而不是emit 一个 worker 读不到的路径。
        std::fs::remove_file(recorder.ca_cert_path()).expect("remove ca.pem");
        assert!(
            recorder.proxy_env().is_empty(),
            "missing CA must skip injection entirely, not emit a broken path"
        );
    }
}
