//! 导入器错误类型。

/// 导入失败原因（单条目问题按 skip 计数，不产生错误）。
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// 源文件 JSON 非法或顶层形态不符。
    #[error("invalid security knowledge source {file}: {detail}")]
    InvalidSource {
        /// 文件名。
        file: String,
        /// 说明。
        detail: String,
    },
    /// 仓储读写失败。
    #[error("storage failure during import: {0}")]
    Storage(#[from] storage::StorageError),
    /// 目录不可读。
    #[error("cannot read directory {path}: {source}")]
    Directory {
        /// 目录路径。
        path: String,
        /// 底层 IO 错误。
        source: std::io::Error,
    },
    /// 语料文件存在但不可读。
    #[error("cannot read source file {path}: {source}")]
    SourceFile {
        /// 文件路径。
        path: String,
        /// 底层 IO 错误。
        source: std::io::Error,
    },
}
