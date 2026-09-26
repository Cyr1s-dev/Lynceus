//! 证书授权（CA）：自签根证书 + 按 host 签发叶证书（MITM 的 TLS 面）。
//!
//! 复用 rcgen 0.14：根证书首次生成后持久化到 `_ca/`（ca.pem + ca.key），
//! 叶证书按 host 现场签发并缓存（`rustls::ServerConfig` 级别，避免每次
//! CONNECT 都重新做一次签名）。

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use rcgen::Issuer;
use rustls::ServerConfig;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;

/// 叶证书缓存上限（超限清空——host 基数小，一次审计碰不到几十个 host）。
const LEAF_CACHE_CAP: usize = 64;

/// 安装进程级 rustls crypto provider（ring）。
///
/// rustls 0.23 在依赖树里同时出现 ring / aws-lc-rs 两个后端时无法自动
/// 判定（reqwest 与 engines 的特性组合就会这样），`ServerConfig` 构建会
/// 直接 panic。代理启动与测试都先走这里，显式选定 ring。
pub fn ensure_crypto_provider() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// 证书授权：根证书 + 按 host 的叶证书签发。
pub struct CertAuthority {
    /// 根证书 PEM（注入子进程信任用）。
    ca_pem: String,
    /// 根证书文件路径。
    ca_cert_path: PathBuf,
    /// rcgen issuer（持根密钥，签叶证书）。
    issuer: Issuer<'static, rcgen::KeyPair>,
    /// host → 预构建的 ServerConfig（含叶证书链 + 私钥）。
    leaves: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertAuthority {
    /// 打开或创建 CA（持久化在 `dir/ca.pem` + `dir/ca.key`）。
    ///
    /// # Errors
    /// 目录创建 / 证书生成 / 读写失败。
    pub fn open(dir: &Path) -> Result<Self, String> {
        ensure_crypto_provider();
        std::fs::create_dir_all(dir).map_err(|error| format!("create ca dir: {error}"))?;
        let ca_cert_path = dir.join("ca.pem");
        let ca_key_path = dir.join("ca.key");

        let (ca_pem, key_pair) = if ca_cert_path.exists() && ca_key_path.exists() {
            let ca_pem = std::fs::read_to_string(&ca_cert_path)
                .map_err(|error| format!("read ca.pem: {error}"))?;
            let key_pem = std::fs::read_to_string(&ca_key_path)
                .map_err(|error| format!("read ca.key: {error}"))?;
            let key_pair = rcgen::KeyPair::from_pem(&key_pem)
                .map_err(|error| format!("parse ca key: {error}"))?;
            (ca_pem, key_pair)
        } else {
            // 首次：生成根证书（CN=Lynceus Traffic CA）并落盘。
            let key_pair = rcgen::KeyPair::generate()
                .map_err(|error| format!("generate ca key: {error}"))?;
            let params = rcgen::CertificateParams::new(vec!["Lynceus Traffic CA".to_string()])
                .map_err(|error| format!("ca params: {error}"))?;
            let mut params = params;
            // 根证书必须是 CA：rcgen 默认 is_ca = NoCa，不加这两个扩展
            // curl/浏览器会把根当普通叶证书拒绝（exit 60）。
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params.key_usages = vec![
                rcgen::KeyUsagePurpose::KeyCertSign,
                rcgen::KeyUsagePurpose::DigitalSignature,
            ];
            params.use_authority_key_identifier_extension = true;
            let cert = params
                .self_signed(&key_pair)
                .map_err(|error| format!("self-sign ca: {error}"))?;
            let ca_pem = cert.pem();
            std::fs::write(&ca_cert_path, &ca_pem)
                .map_err(|error| format!("write ca.pem: {error}"))?;
            std::fs::write(&ca_key_path, key_pair.serialize_pem())
                .map_err(|error| format!("write ca.key: {error}"))?;
            (ca_pem, key_pair)
        };

        // Issuer 借 PEM 的生命周期；CA 是进程级单例，漏一份静态副本。
        let ca_pem_static: &'static str = Box::leak(ca_pem.clone().into_boxed_str());
        let issuer = Issuer::from_ca_cert_pem(ca_pem_static, key_pair)
            .map_err(|error| format!("build ca issuer: {error}"))?;

        Ok(Self {
            ca_pem,
            ca_cert_path,
            issuer,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// 根证书 PEM。
    #[must_use]
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// 根证书文件路径。
    #[must_use]
    pub fn ca_cert_path(&self) -> PathBuf {
        self.ca_cert_path.clone()
    }

    /// 取（或签发）该 host 的 TLS server config。
    ///
    /// # Errors
    /// 叶证书签发 / rustls 配置构建失败。
    pub fn server_config_for(&self, host: &str) -> Result<Arc<ServerConfig>, String> {
        if let Some(config) = self
            .leaves
            .lock()
            .map_err(|_| "leaf cache poisoned".to_string())?
            .get(host)
        {
            return Ok(Arc::clone(config));
        }
        let config = Arc::new(self.sign_leaf(host)?);
        let mut cache = self
            .leaves
            .lock()
            .map_err(|_| "leaf cache poisoned".to_string())?;
        if cache.len() >= LEAF_CACHE_CAP {
            cache.clear();
        }
        cache.insert(host.to_string(), Arc::clone(&config));
        Ok(config)
    }

    /// 为 host 签发叶证书并构建 `rustls::ServerConfig`。
    fn sign_leaf(&self, host: &str) -> Result<ServerConfig, String> {
        let key_pair = rcgen::KeyPair::generate()
            .map_err(|error| format!("generate leaf key: {error}"))?;
        let mut params = rcgen::CertificateParams::new(vec![host.to_string()])
            .map_err(|error| format!("leaf params: {error}"))?;
        // 叶证书长期有效（代理重启后已签发的仍可用；根证书轮换时全量重签）。
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2035, 1, 1);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ];
        params.use_authority_key_identifier_extension = true;
        let cert = params
            .signed_by(&key_pair, &self.issuer)
            .map_err(|error| format!("sign leaf for {host}: {error}"))?;
        let leaf_pem = cert.pem();
        let leaf_key_pem = key_pair.serialize_pem();

        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(leaf_pem.as_bytes())
            .filter_map(Result::ok)
            .collect();
        if certs.is_empty() {
            return Err(format!("no certificate parsed from leaf pem for {host}"));
        }
        let key = PrivateKeyDer::from_pem_slice(leaf_key_pem.as_bytes())
            .map_err(|error| format!("parse leaf key for {host}: {error}"))?;

        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| format!("build server config for {host}: {error}"))?;
        // 只服务 HTTP/1.1（MITM 面不做 h2，规避 ALPN 复杂性）。
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_persists_and_signs_per_host_leaves() {
        ensure_crypto_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = CertAuthority::open(dir.path()).expect("ca opens");
        assert!(ca.ca_cert_path().exists(), "ca.pem must persist");
        assert!(ca.ca_pem().contains("BEGIN CERTIFICATE"));

        // 同 host 命中缓存（同一 Arc）。
        let first = ca.server_config_for("example.test").expect("leaf");
        let second = ca.server_config_for("example.test").expect("leaf");
        assert!(Arc::ptr_eq(&first, &second), "同 host 必须命中缓存");

        // 异 host 各自签发。
        let other = ca
            .server_config_for("other.test")
            .expect("leaf for other host");
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn ca_reopens_from_persisted_files() {
        ensure_crypto_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let first = CertAuthority::open(dir.path()).expect("ca opens");
        let pem = first.ca_pem().to_string();
        // 二次 open 必须复用同一根证书（否则 worker 信任的 CA 会变）。
        let second = CertAuthority::open(dir.path()).expect("ca reopens");
        assert_eq!(second.ca_pem(), pem, "根证书必须持久复用");
    }
}
