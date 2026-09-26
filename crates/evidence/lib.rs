//! 证据工件与出处绑定的指纹。
//!
//! `server/core/evidence/`（`Guardian`、`Provenance Gate`、`artifact_repair`）的
//! Rust 移植起点。本 crate 存在的直接理由是一条生产事故红线：Python 的
//! 文本模式 `write_text` 在 Windows 上把 `\n` 转成 `\r\n`，内存字节的
//! SHA-256 与磁盘字节不再一致，Finding 因此卡在 `needs_review`、Mission
//! 误判未完成。Rust 的 [`std::fs::write`] 在所有平台按原始字节落盘，天然
//! 免疫该 bug；[`SealedArtifact`] 进一步把内容与指纹在类型层绑定，使
//! “先建后补指纹”“指纹与字节不一致”不可表示。
//!
//! # 红线
//!
//! - 工件一律字节精确落盘：任何平台都不做换行/编码转换；
//! - 指纹 = 磁盘上真实字节的 SHA-256，绝不基于转换后内容计算；
//! - [`SealedArtifact::persist`] 与 [`SealedArtifact::load_verified`] 的
//!   字节同源（同一个 `bytes` 缓冲）。
//!
//! # 示例
//!
//! ```
//! use evidence::SealedArtifact;
//!
//! let artifact = SealedArtifact::seal(b"proof\r\nwith crlf\n".to_vec());
//! assert_eq!(artifact.bytes(), b"proof\r\nwith crlf\n");
//! assert_eq!(artifact.sha256().as_hex().len(), 64);
//! ```

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod guardian;
pub mod provenance_gate;
pub mod verification;

pub use guardian::{Guardian, GuardianVerdict};
pub use provenance_gate::{ProvenanceDecision, ProvenanceGate};
pub use verification::{
    FindingVerificationDecision, FindingVerificationService, ProductVerification,
};

use std::fmt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 出处校验错误：一个领域失败模式一个变体。
#[derive(Debug, thiserror::Error)]
pub enum ProvenanceError {
    /// 磁盘字节与期望指纹不符（历史上的换行污染、篡改、截断都会落到这里）。
    #[error("artifact {path} fingerprint mismatch: expected {expected}, got {actual}")]
    FingerprintMismatch {
        /// 工件路径。
        path: PathBuf,
        /// 期望的 SHA-256（小写十六进制）。
        expected: String,
        /// 实际计算出的 SHA-256（小写十六进制）。
        actual: String,
    },
    /// 工件文件在磁盘上不存在。
    #[error("artifact {0} missing from disk")]
    ArtifactMissing(PathBuf),
    /// 传入的指纹字符串不是合法的 64 位小写十六进制。
    #[error("invalid SHA-256 fingerprint {value:?}: expected 64 lowercase hex characters")]
    InvalidFingerprint {
        /// 被拒绝的原始字符串。
        value: String,
    },
    /// 工件读写失败（权限、磁盘等 I/O 层失败）。
    #[error("artifact I/O failed on {path}: {source}")]
    Io {
        /// 工件路径。
        path: PathBuf,
        /// 底层 I/O 错误。
        #[source]
        source: std::io::Error,
    },
}

/// 对磁盘真实字节计算的 SHA-256 指纹（小写十六进制，64 字符）。
///
/// 语义边界：指纹只对“落盘的字节”有意义，绝不基于转换后的内容计算；
/// 小写十六进制与 Python `hashlib.hexdigest()` 输出一致，大写形式被
/// 视为非法以暴露上游规范化漂移。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Fingerprint(String);

impl Sha256Fingerprint {
    /// 计算字节的 SHA-256 指纹。
    #[must_use]
    pub fn compute(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(to_hex(&digest))
    }

    /// 从 64 位小写十六进制字符串解析指纹（来自数据库或 API 的期望值）。
    ///
    /// # Errors
    /// - [`ProvenanceError::InvalidFingerprint`]：长度不是 64，或包含
    ///   非小写十六进制字符。大写被拒绝，以暴露上游规范化漂移。
    pub fn from_hex(hex: &str) -> Result<Self, ProvenanceError> {
        if hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(hex.to_string()))
        } else {
            Err(ProvenanceError::InvalidFingerprint {
                value: hex.to_string(),
            })
        }
    }

    /// 小写十六进制表示。
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 内容与指纹绑定的证据工件。
///
/// 不变式：`sha256` 恒等于 `bytes` 的 SHA-256。字段私有、只能经
/// [`SealedArtifact::seal`] 构造，因此不存在“先建后补指纹”的路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedArtifact {
    bytes: Vec<u8>,
    sha256: Sha256Fingerprint,
}

impl SealedArtifact {
    /// 封装工件字节并计算指纹。
    ///
    /// ```
    /// use evidence::{SealedArtifact, Sha256Fingerprint};
    ///
    /// let artifact = SealedArtifact::seal(b"abc".to_vec());
    /// assert_eq!(
    ///     artifact.sha256().as_hex(),
    ///     "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    /// );
    /// assert_eq!(artifact.sha256(), &Sha256Fingerprint::compute(b"abc"));
    /// ```
    #[must_use]
    pub fn seal(bytes: Vec<u8>) -> Self {
        let sha256 = Sha256Fingerprint::compute(&bytes);
        Self { bytes, sha256 }
    }

    /// 工件原始字节（与指纹计算、落盘写入同源）。
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 工件指纹。
    #[must_use]
    pub fn sha256(&self) -> &Sha256Fingerprint {
        &self.sha256
    }

    /// 字节精确落盘：写入 `bytes()` 的原始字节，不做任何转换。
    ///
    /// 不自动创建父目录——目录结构属调用方职责，保持本函数纯写语义。
    ///
    /// # Errors
    /// - [`ProvenanceError::Io`]：写入失败（权限、磁盘满、父目录缺失）。
    pub fn persist(&self, path: &Path) -> Result<(), ProvenanceError> {
        std::fs::write(path, &self.bytes).map_err(|source| ProvenanceError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// 从磁盘读取工件并按期望指纹校验。
    ///
    /// # Errors
    /// - [`ProvenanceError::ArtifactMissing`]：文件不存在；
    /// - [`ProvenanceError::FingerprintMismatch`]：磁盘字节与期望指纹不符；
    /// - [`ProvenanceError::Io`]：其他读失败。
    pub fn load_verified(
        path: &Path,
        expected: &Sha256Fingerprint,
    ) -> Result<Self, ProvenanceError> {
        let bytes = std::fs::read(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ProvenanceError::ArtifactMissing(path.to_path_buf())
            } else {
                ProvenanceError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;
        let actual = Sha256Fingerprint::compute(&bytes);
        if &actual != expected {
            return Err(ProvenanceError::FingerprintMismatch {
                path: path.to_path_buf(),
                expected: expected.as_hex().to_string(),
                actual: actual.as_hex().to_string(),
            });
        }
        Ok(Self {
            bytes,
            sha256: actual,
        })
    }
}

/// 小写十六进制编码（与 `hashlib.hexdigest()` 一致）。
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 以 Python `repr(list[str])` 的形态格式化字符串列表（`['a', 'b']`）。
///
/// 门禁原因文本会进入 `finding.review` JSON 并经 API 暴露，双跑期间两侧
/// 必须逐字节一致——Python f-string 对列表的插值即 repr。覆盖域为可打印
/// ASCII（ID、固定短语），转义规则与 `CPython` 一致（`\` `\n` `\r` `\t`
/// 与定界引号）。
pub(crate) fn python_list_repr<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::from("[");
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&python_repr_str(item));
    }
    out.push(']');
    out
}

/// 单个字符串的 Python `repr` 形态。
fn python_repr_str(value: &str) -> String {
    let has_single = value.contains('\'');
    let has_double = value.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("系统临时目录应可创建")
    }

    #[test]
    fn sha256_known_vector() {
        // FIPS 180-2 标准测试向量，防止哈希管线被误改。
        let fingerprint = Sha256Fingerprint::compute(b"abc");
        assert_eq!(
            fingerprint.as_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn from_hex_accepts_exactly_64_lowercase_hex() {
        let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let parsed = Sha256Fingerprint::from_hex(hex).expect("合法小写十六进制应被接受");
        assert_eq!(parsed.as_hex(), hex);

        assert!(Sha256Fingerprint::from_hex("").is_err(), "空串必须拒绝");
        assert!(
            Sha256Fingerprint::from_hex(&hex[..63]).is_err(),
            "长度不足必须拒绝"
        );
        assert!(
            Sha256Fingerprint::from_hex(&format!("{hex}0")).is_err(),
            "超长必须拒绝"
        );
        assert!(
            Sha256Fingerprint::from_hex(&hex.to_uppercase()).is_err(),
            "大写必须拒绝：暴露上游规范化漂移"
        );
        assert!(
            Sha256Fingerprint::from_hex(&hex.replace('a', "g")).is_err(),
            "非十六进制字符必须拒绝"
        );
    }

    #[test]
    fn crlf_lf_mixed_content_roundtrips_byte_exact() {
        // 历史事故复现输入：混合 CRLF/LF 的内容不得被任何一层转换。
        let bytes: Vec<u8> = b"line1\r\nline2\nline3\r\r\n\nmixed \xc3\xa9".to_vec();
        let artifact = SealedArtifact::seal(bytes.clone());
        let dir = temp_dir();
        let path = dir.path().join("evidence.txt");

        artifact.persist(&path).expect("落盘不应失败");
        assert_eq!(
            std::fs::metadata(&path).expect("落盘后文件应存在").len(),
            bytes.len() as u64,
            "文件长度必须与字节长度一致：换行污染会在此暴露"
        );

        let loaded =
            SealedArtifact::load_verified(&path, artifact.sha256()).expect("同源字节必须校验通过");
        assert_eq!(loaded.bytes(), bytes.as_slice());
        assert_eq!(loaded.sha256(), artifact.sha256());
    }

    #[test]
    fn tampered_file_fails_verification() {
        let artifact = SealedArtifact::seal(b"original evidence".to_vec());
        let dir = temp_dir();
        let path = dir.path().join("tampered.txt");
        artifact.persist(&path).expect("落盘不应失败");

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("追加写入应成功");
        file.write_all(b"tampered").expect("追加写入应成功");
        drop(file);

        let error = SealedArtifact::load_verified(&path, artifact.sha256())
            .expect_err("被篡改的文件必须校验失败");
        assert!(matches!(error, ProvenanceError::FingerprintMismatch { .. }));
    }

    #[test]
    fn missing_file_reports_artifact_missing() {
        let dir = temp_dir();
        let path = dir.path().join("nonexistent.bin");
        let expected = Sha256Fingerprint::compute(b"anything");
        let error =
            SealedArtifact::load_verified(&path, &expected).expect_err("不存在的文件必须报错");
        assert!(matches!(error, ProvenanceError::ArtifactMissing(_)));
    }

    #[test]
    fn persist_into_missing_directory_reports_io() {
        let artifact = SealedArtifact::seal(b"x".to_vec());
        let dir = temp_dir();
        let path = dir.path().join("missing-dir").join("out.bin");
        let error = artifact
            .persist(&path)
            .expect_err("父目录缺失必须报 Io 而不是 panic");
        assert!(matches!(error, ProvenanceError::Io { .. }));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// 任意字节（含随机 \r\n 序列与非法 UTF-8）落盘后必须逐字节还原，
        /// 且指纹保持一致。这是对“换行污染”红线的属性级锁定。
        #[test]
        fn write_then_read_is_byte_exact(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let artifact = SealedArtifact::seal(bytes.clone());
            let dir = tempfile::tempdir().expect("系统临时目录应可创建");
            let path = dir.path().join("artifact.bin");

            artifact.persist(&path).expect("原始字节落盘不应失败");
            prop_assert_eq!(std::fs::metadata(&path).expect("文件应存在").len(), bytes.len() as u64);

            let loaded = SealedArtifact::load_verified(&path, artifact.sha256())
                .expect("同源字节必须校验通过");
            prop_assert_eq!(loaded.bytes(), bytes.as_slice());
            prop_assert_eq!(loaded.sha256(), artifact.sha256());
        }

        /// 指纹十六进制表示可无损解析回来。
        #[test]
        fn fingerprint_hex_roundtrip(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let fingerprint = Sha256Fingerprint::compute(&bytes);
            let parsed = Sha256Fingerprint::from_hex(fingerprint.as_hex())
                .expect("compute 的输出必须能被 from_hex 解析");
            prop_assert_eq!(parsed, fingerprint);
        }

        /// 任意一位翻转必须改变指纹（SHA-256 雪崩性的工程级验证）。
        #[test]
        fn single_bit_flip_changes_fingerprint(
            bytes in proptest::collection::vec(any::<u8>(), 1..256),
            bit in 0usize..2048,
        ) {
            let mut flipped = bytes.clone();
            let byte_index = bit / 8;
            if byte_index >= flipped.len() {
                // 输入长度小于位索引时翻最后一个字节，保证总在界内。
                let last = flipped.len() - 1;
                flipped[last] ^= 1 << (bit % 8);
            } else {
                flipped[byte_index] ^= 1 << (bit % 8);
            }
            prop_assert_ne!(
                Sha256Fingerprint::compute(&bytes),
                Sha256Fingerprint::compute(&flipped)
            );
        }
    }
}
