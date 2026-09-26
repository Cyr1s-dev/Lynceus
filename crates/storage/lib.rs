//! Lynceus 存储层 —— `server/core/storage/` 的 Rust 移植。
//!
//! - [`schema`]：全量物理模式（46 表 + 索引），与 Python `_init_schema`
//!   逐句对齐，双跑期间两侧打开同一个数据库文件；
//! - [`repository`]：[`Repository`] 协议（`repository.py` 的阶段 1 子集，
//!   覆盖核心对象链 7 实体）与 [`Storable`] 实体→物理布局映射；
//! - [`sqlite`]：[`SqliteRepository`]，rusqlite 实现，SQL 照搬 Python 侧；
//! - [`dump`]：差分对拍 harness 依赖的规范化 SQLite dump；
//! - [`fixture`]：跨语言对拍 fixture（固定输入的确定性仓储操作序列，
//!   与 `scripts/parity_fixture.py` 逐操作镜像）。
//!
//! 时间戳编码约定（两侧必须一致，见 `models::Timestamp`）：
//! `payload` 列存 wire 格式（UTC 后缀 `Z`），`created_at` 列存
//! `datetime.isoformat()`（UTC 后缀 `+00:00`）——两种格式不可混用。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod dump;
pub mod error;
pub mod fixture;
pub mod journal;
pub mod knowledge_aliases;
pub mod knowledge_fts;
pub mod knowledge_packing;
pub mod knowledge_query;
pub mod knowledge_search;
pub mod redaction;
pub mod repository;
pub mod schema;
pub mod sqlite;

pub use dump::dump_database;
pub use error::StorageError;
pub use journal::{
    JournalError, PageQuery, SwarmOperationJournal, SwarmOperationPage, SwarmOperationRecord,
    normalize, parse_line, resolve_mission_operation_log_dir,
};
pub use knowledge_aliases::AliasRegistry;
pub use knowledge_fts::{
    BM25_COLUMN_WEIGHTS, assemble_results, corpus_status, ensure_knowledge_fts,
    migrate_legacy_cards, rebuild, search as fts_search, upsert_card as fts_upsert_card,
};
pub use knowledge_packing::{PackOptions, PackedKnowledge, pack_knowledge};
pub use knowledge_query::{derive_search_terms, fts_match_expr, normalize_query};
pub use knowledge_search::search_cards;
pub use redaction::{redact_text, redact_value};
pub use repository::Repository;
pub use repository::Storable;
pub use sqlite::SqliteRepository;
