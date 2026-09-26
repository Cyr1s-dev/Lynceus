//! 知识卡 FTS5 索引 —— BM25 检索与语料状态管理。
//!
//! 设计要点：
//! * **standalone FTS5 表**（`knowledge_fts`，unicode61 tokenizer），由
//!   写入路径增量维护（add → upsert；delete → remove），并提供全量
//!   `rebuild`（index-sync / migration / 批量导入后调用）——不依赖
//!   trigger 解析 JSON payload，行为可测；
//! * 字段权重遵循 `title/aliases/technique/tags/tool > summary > body`
//!   的概念优先级，通过 `bm25()` 列权重实现，真值可由 retrieval eval
//!   调整（常量集中在此处）；
//! * **ingestion 期 `search_terms`**：索引时从标题/摘要/别名/结构化
//!   字段派生归一化词（含 CJK 二元组与别名展开）写入专用列——这是
//!   unicode61 下中文召回的根基；query 侧同样归一化后以 OR 匹配；
//! * 语料状态（empty/ready/stale）由卡片数、索引行数与最近更新时间
//!   对比 `knowledge_index_meta` 中记录的同步点得出，杜绝"数据已更新
//!   而索引静默过期"。

use rusqlite::Connection;

use models::knowledge::{KnowledgeCard, KnowledgeCorpusState, KnowledgeCorpusStatus};

use crate::StorageError;
use crate::knowledge_aliases::AliasRegistry;
use crate::knowledge_query::{fts_match_expr, normalize_query};

/// FTS5 虚表 DDL（幂等）。
const KNOWLEDGE_FTS_DDL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(
    id UNINDEXED,
    title,
    summary,
    body,
    aliases,
    tags,
    tool,
    technique,
    platform,
    protocol,
    search_terms,
    tokenize = 'unicode61 remove_diacritics 2'
)";

/// 同步点元表（幂等；游离于 46 表守护集合之外的知识域专用设施）。
const KNOWLEDGE_INDEX_META_DDL: &str = "CREATE TABLE IF NOT EXISTS knowledge_index_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
)";

/// `bm25()` 列权重（顺序 = 虚表列序）。正值 = 越重要。
/// title/aliases/technique/tags/tool > summary > body 的概念优先级。
pub const BM25_COLUMN_WEIGHTS: [f64; 10] = [
    3.0, // title
    3.0, // aliases
    3.0, // tool
    3.0, // technique
    2.5, // tags
    2.0, // summary
    2.0, // search_terms
    1.5, // platform
    1.5, // protocol
    1.0, // body
];

/// 确保知识域 FTS 与元表存在（幂等，知识读写路径入口调用）。
///
/// # Errors
/// DDL 执行失败。
pub fn ensure_knowledge_fts(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute(KNOWLEDGE_FTS_DDL, [])
        .map_err(|source| StorageError::Schema {
            table: "knowledge_fts".to_string(),
            source,
        })?;
    connection
        .execute(KNOWLEDGE_INDEX_META_DDL, [])
        .map_err(|source| StorageError::Schema {
            table: "knowledge_index_meta".to_string(),
            source,
        })?;
    Ok(())
}

/// 索引行文本：`(title, summary, body, aliases, tags, tool, technique,
/// platform, protocol, search_terms)`。
///
/// `search_terms` 在卡片未携带时由注册表派生（migration / 老数据兼容）。
#[must_use]
pub fn fts_row_for(card: &KnowledgeCard, registry: &AliasRegistry) -> [String; 10] {
    let summary = card.effective_summary().to_string();
    let aliases = card.aliases.join(" ");
    let tags = card.tags.join(" ");
    let tool = card.tool.join(" ");
    let technique = card.technique.join(" ");
    let platform = card.platform.join(" ");
    let protocol = card.protocol.join(" ");
    let search_terms = if card.search_terms.is_empty() {
        derive_card_search_terms(card, registry).join(" ")
    } else {
        card.search_terms.join(" ")
    };
    [
        card.title.clone(),
        summary,
        card.body.clone(),
        aliases,
        tags,
        tool,
        technique,
        platform,
        protocol,
        search_terms,
    ]
}

/// ingestion 期检索词派生：标题 + 摘要 + 别名 + 结构化字段 → 归一化 +
/// 别名展开 + CJK 二元组（经 `normalize_query`）。
#[must_use]
pub fn derive_card_search_terms(card: &KnowledgeCard, registry: &AliasRegistry) -> Vec<String> {
    let fields: Vec<String> = [
        Some(card.title.clone()),
        Some(card.effective_summary().to_string()),
        (!card.aliases.is_empty()).then(|| card.aliases.join(" ")),
        (!card.tags.is_empty()).then(|| card.tags.join(" ")),
        (!card.tool.is_empty()).then(|| card.tool.join(" ")),
        (!card.technique.is_empty()).then(|| card.technique.join(" ")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let field_refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    normalize_query(&field_refs.join(" "), registry).all_terms
}

/// 写入/更新一张卡的索引行（`id` 为键，先删后插）。
///
/// # Errors
/// SQL 执行失败。
pub fn upsert_card(
    connection: &Connection,
    card: &KnowledgeCard,
    registry: &AliasRegistry,
) -> Result<(), StorageError> {
    ensure_knowledge_fts(connection)?;
    connection
        .execute("DELETE FROM knowledge_fts WHERE id = ?", [card.id.as_str()])
        .map_err(write_error("knowledge_fts(delete)"))?;
    let row = fts_row_for(card, registry);
    connection
        .execute(
            "INSERT INTO knowledge_fts (id, title, summary, body, aliases, tags, tool,
             technique, platform, protocol, search_terms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                card.id.as_str(),
                row[0],
                row[1],
                row[2],
                row[3],
                row[4],
                row[5],
                row[6],
                row[7],
                row[8],
                row[9],
            ],
        )
        .map_err(write_error("knowledge_fts(insert)"))?;
    Ok(())
}

/// 移除一张卡的索引行。
///
/// # Errors
/// SQL 执行失败。
pub fn remove_card(connection: &Connection, card_id: &str) -> Result<(), StorageError> {
    ensure_knowledge_fts(connection)?;
    connection
        .execute("DELETE FROM knowledge_fts WHERE id = ?", [card_id])
        .map_err(write_error("knowledge_fts(delete)"))?;
    Ok(())
}

/// 全量重建索引（index-sync / migration / 批量导入后）。返回索引行数。
///
/// # Errors
/// SQL 执行失败。
pub fn rebuild(
    connection: &Connection,
    cards: &[KnowledgeCard],
    registry: &AliasRegistry,
) -> Result<usize, StorageError> {
    ensure_knowledge_fts(connection)?;
    connection
        .execute("DELETE FROM knowledge_fts", [])
        .map_err(write_error("knowledge_fts(clear)"))?;
    for card in cards {
        upsert_card(connection, card, registry)?;
    }
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM knowledge_fts", [], |row| row.get(0))
        .map_err(write_error("knowledge_fts(count)"))?;
    let synced_card_count_value = count.to_string();
    write_meta(
        connection,
        &[
            ("synced_at", Some(now_iso().as_str())),
            ("synced_card_count", Some(synced_card_count_value.as_str())),
        ],
    )?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// FTS 检索：`MATCH` + BM25 排序，返回 `(card_id, 正向化得分)`。
///
/// 得分 = `-bm25(weights)`（bm25 越负越相关，转正后越大越好）。
/// `fetch_limit` 应大于最终 `limit`（候选拉取倍率），结构化过滤在
/// 载入 payload 后由调用方执行。
///
/// # Errors
/// SQL 执行失败。
pub fn search(
    connection: &Connection,
    query_text: &str,
    fetch_limit: usize,
    registry: &AliasRegistry,
) -> Result<Vec<(String, f64)>, StorageError> {
    ensure_knowledge_fts(connection)?;
    let normalized = normalize_query(query_text, registry);
    let Some(match_expr) = fts_match_expr(&normalized) else {
        return Ok(Vec::new());
    };
    let weights: Vec<String> = BM25_COLUMN_WEIGHTS
        .iter()
        .map(ToString::to_string)
        .collect();
    let sql = format!(
        "SELECT id, bm25(knowledge_fts, {}) AS rank FROM knowledge_fts \
         WHERE knowledge_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        weights.join(", ")
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(write_error("knowledge_fts(search)"))?;
    let rows = statement
        .query_map(
            rusqlite::params![match_expr, i64::try_from(fetch_limit).unwrap_or(i64::MAX)],
            |row| {
                let id: String = row.get(0)?;
                let rank: f64 = row.get(1)?;
                Ok((id, -rank))
            },
        )
        .map_err(write_error("knowledge_fts(search)"))?;
    let mut hits = Vec::new();
    for row in rows {
        let (id, score) = row.map_err(write_error("knowledge_fts(search)"))?;
        hits.push((id, score));
    }
    Ok(hits)
}

/// 语料状态：对比卡片数 / 索引行数 / 最近更新与同步点。
///
/// # Errors
/// SQL 执行失败。
pub fn corpus_status(connection: &Connection) -> Result<KnowledgeCorpusStatus, StorageError> {
    ensure_knowledge_fts(connection)?;
    let card_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM knowledge_cards", [], |row| row.get(0))
        .map_err(write_error("knowledge_cards(count)"))?;
    let indexed_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM knowledge_fts", [], |row| row.get(0))
        .map_err(write_error("knowledge_fts(count)"))?;
    let synced_at: Option<String> = read_meta(connection, "synced_at")?;
    let last_synced_at: Option<models::Timestamp> =
        synced_at.as_ref().and_then(|value| value.parse().ok());
    let synced_card_count: Option<i64> =
        read_meta(connection, "synced_card_count")?.and_then(|value| value.parse().ok());
    let latest_updated: Option<String> = connection
        .query_row(
            "SELECT MAX(json_extract(payload, '$.updated_at')) FROM knowledge_cards",
            [],
            |row| row.get(0),
        )
        .map_err(write_error("knowledge_cards(max_updated)"))?;

    let (state, reason) = if card_count == 0 {
        (
            KnowledgeCorpusState::Empty,
            "no knowledge cards stored".to_string(),
        )
    } else if indexed_count == 0 {
        (
            KnowledgeCorpusState::Stale,
            "cards exist but the FTS index is empty; run knowledge index-sync".to_string(),
        )
    } else if synced_card_count != Some(card_count)
        || latest_updated
            .as_ref()
            .is_some_and(|updated| synced_at.as_ref().is_none_or(|synced| updated > synced))
    {
        (
            KnowledgeCorpusState::Stale,
            "corpus changed since the last index sync; run knowledge index-sync".to_string(),
        )
    } else {
        (
            KnowledgeCorpusState::Ready,
            "index is in sync with the corpus".to_string(),
        )
    };
    Ok(KnowledgeCorpusStatus {
        state,
        card_count,
        indexed_count,
        last_synced_at,
        reason,
    })
}

/// 按命中序载入卡片 payload（保持 BM25 排序；缺失 id 静默跳过——索引
/// 与语料的瞬时漂移由 `corpus_status` 负责暴露）。
///
/// # Errors
/// payload 反序列化失败或 SQL 执行失败。
pub fn load_cards_by_ids(
    connection: &Connection,
    hits: &[(String, f64)],
) -> Result<Vec<KnowledgeCard>, StorageError> {
    let mut cards = Vec::with_capacity(hits.len());
    for (id, _) in hits {
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload FROM knowledge_cards WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(query_error("knowledge_cards"))?;
        if let Some(payload) = payload {
            let card: KnowledgeCard =
                serde_json::from_str(&payload).map_err(|source| StorageError::Decode {
                    table: format!("knowledge_cards({id})"),
                    source,
                })?;
            cards.push(card);
        }
    }
    Ok(cards)
}

/// 把 FTS 命中（BM25 序）与卡片 payload 组装为检索结果。
///
/// * 结构化过滤（kinds/tags/tools/techniques/platforms/protocols）在
///   payload 上执行（大小写不敏感），全空 = 不过滤；
/// * `matched_terms` = 归一化词项在卡片可检索文本中的实际命中；
/// * `retrieval_reason` 按确定性优先级分类（exact id > title > alias >
///   technique > tag > summary > body > bm25）。
#[must_use]
#[allow(clippy::cast_precision_loss)] // 计数远小于 2^53，`as f64` 无精度损失。
pub fn assemble_results(
    hits: &[(String, f64)],
    cards: &[KnowledgeCard],
    query: &models::knowledge::KnowledgeRetrievalQuery,
    registry: &AliasRegistry,
) -> Vec<models::knowledge::KnowledgeRetrievalResult> {
    use models::knowledge::KnowledgeRetrievalResult;

    let normalized = normalize_query(&query.text, registry);
    let score_by_id: std::collections::HashMap<&str, f64> = hits
        .iter()
        .map(|(id, score)| (id.as_str(), *score))
        .collect();
    let mut results = Vec::new();
    for card in cards {
        let Some(&score) = score_by_id.get(card.id.as_str()) else {
            continue;
        };
        if !passes_structured_filters(card, query) {
            continue;
        }
        let searchable = searchable_text(card).to_lowercase();
        let matched: Vec<String> = normalized
            .all_terms
            .iter()
            .filter(|term| searchable.contains(term.as_str()))
            .cloned()
            .collect();
        let reason = classify_reason(card, &matched, &normalized.alias_expansions);
        results.push(KnowledgeRetrievalResult {
            card: card.clone(),
            score,
            matched_terms: matched,
            retrieval_reason: Some(reason),
        });
        if results.len() >= usize::try_from(query.limit.max(0)).unwrap_or(usize::MAX) {
            break;
        }
    }
    results
}

/// 纯过滤查询（无文本词项）的确定性结果：priority/100 评分 + 过滤。
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn filter_only_results(
    cards: &[KnowledgeCard],
    query: &models::knowledge::KnowledgeRetrievalQuery,
) -> Vec<models::knowledge::KnowledgeRetrievalResult> {
    use models::knowledge::KnowledgeRetrievalResult;

    let mut filtered: Vec<&KnowledgeCard> = cards
        .iter()
        .filter(|card| passes_structured_filters(card, query))
        .collect();
    filtered.sort_by(|a, b| {
        let key = |card: &KnowledgeCard| (card.priority, card.created_at);
        key(b).cmp(&key(a))
    });
    filtered
        .into_iter()
        .take(usize::try_from(query.limit.max(0)).unwrap_or(usize::MAX))
        .map(|card| KnowledgeRetrievalResult {
            card: card.clone(),
            score: card.priority as f64 / 100.0,
            matched_terms: Vec::new(),
            retrieval_reason: Some("structured_filter".to_string()),
        })
        .collect()
}

/// 结构化过滤（空集合 = 不限；比较大小写不敏感）。
fn passes_structured_filters(
    card: &KnowledgeCard,
    query: &models::knowledge::KnowledgeRetrievalQuery,
) -> bool {
    let contains_any = |haystack: &[String], needles: &[String]| -> bool {
        needles.is_empty()
            || needles.iter().any(|needle| {
                let needle = needle.to_lowercase();
                haystack.iter().any(|item| item.to_lowercase() == needle)
            })
    };
    let tag_contains = |needles: &[String]| -> bool {
        needles.is_empty()
            || needles.iter().any(|needle| {
                let needle = needle.to_lowercase();
                card.tags.iter().any(|tag| tag.to_lowercase() == needle)
            })
    };
    contains_any(&card.tool, &query.tools)
        && contains_any(&card.technique, &query.techniques)
        && contains_any(&card.platform, &query.platforms)
        && contains_any(&card.protocol, &query.protocols)
        && tag_contains(&query.tags)
        && (query.kinds.is_empty() || query.kinds.contains(&card.kind))
}

/// 卡片可检索文本（与 FTS 行一致的语料超集）。
fn searchable_text(card: &KnowledgeCard) -> String {
    [
        card.title.as_str(),
        card.effective_summary(),
        card.body.as_str(),
        card.aliases.join(" ").as_str(),
        card.tags.join(" ").as_str(),
        card.tool.join(" ").as_str(),
        card.technique.join(" ").as_str(),
        card.platform.join(" ").as_str(),
        card.protocol.join(" ").as_str(),
        card.search_terms.join(" ").as_str(),
    ]
    .join(" ")
}

/// deterministic 检索原因分类（供 UI / 评测 / mission trace）。
fn classify_reason(
    card: &KnowledgeCard,
    matched: &[String],
    alias_expansions: &[(String, String)],
) -> String {
    let title = card.title.to_lowercase();
    let summary = card.effective_summary().to_lowercase();
    let body = card.body.to_lowercase();
    let aliases = card
        .aliases
        .iter()
        .map(|item| item.to_lowercase())
        .collect::<Vec<_>>();
    let tags = card
        .tags
        .iter()
        .map(|item| item.to_lowercase())
        .collect::<Vec<_>>();
    let techniques = card
        .technique
        .iter()
        .chain(card.tool.iter())
        .map(|item| item.to_lowercase())
        .collect::<Vec<_>>();
    let in_list = |list: &[String], term: &str| list.iter().any(|item| item.contains(term));

    let expanded: Vec<&str> = alias_expansions
        .iter()
        .map(|(term, _)| term.as_str())
        .collect();
    if matched
        .iter()
        .any(|term| term.starts_with("cve-") || term.starts_with("cwe-"))
    {
        return "exact_identifier_match".to_string();
    }
    if matched.iter().any(|term| title.contains(term.as_str())) {
        return "title_match".to_string();
    }
    if matched.iter().any(|term| in_list(&aliases, term))
        || matched.iter().any(|term| expanded.contains(&term.as_str()))
    {
        return "alias_match".to_string();
    }
    if matched.iter().any(|term| in_list(&techniques, term)) {
        return "technique_match".to_string();
    }
    if matched.iter().any(|term| in_list(&tags, term)) {
        return "tag_match".to_string();
    }
    if matched.iter().any(|term| summary.contains(term.as_str())) {
        return "summary_match".to_string();
    }
    if matched.iter().any(|term| body.contains(term.as_str())) {
        return "body_match".to_string();
    }
    "bm25_match".to_string()
}

/// 确定性 legacy 迁移（§39，不依赖 LLM）：
///
/// * 旧卡（只有 `content`）→ `summary = content`、`body = content`
///   （summary fallback 与完整知识同源，注入语义不变）；
/// * `search_terms` 缺失的卡 → 派生归一化检索词；
/// * `content_hash` 缺失 → 按 body（缺省 summary）补 SHA-256；
/// * 迁移行走 `add_knowledge_card` upsert，FTS 索引随写维护。调用方
///   之后可调 [`rebuild`] 收口同步点。返回迁移行数。
///
/// # Errors
/// 仓储读写失败。
pub fn migrate_legacy_cards(repository: &dyn crate::Repository) -> Result<usize, StorageError> {
    let cards = repository.list_knowledge_cards()?;
    let registry = AliasRegistry::embedded();
    let mut migrated = 0usize;
    for mut card in cards {
        let needs_split = card.summary.is_empty() && !card.content.is_empty();
        let needs_terms = card.search_terms.is_empty();
        let needs_hash = card.content_hash.is_none();
        if !needs_split && !needs_terms && !needs_hash {
            continue;
        }
        if needs_split {
            card.summary = card.content.clone();
            card.body = card.content.clone();
        }
        if needs_terms {
            let aliases = card.aliases.join(" ");
            let tags = card.tags.join(" ");
            card.search_terms = crate::knowledge_query::derive_search_terms(
                &[
                    card.title.as_str(),
                    card.effective_summary(),
                    aliases.as_str(),
                    tags.as_str(),
                ],
                registry,
            );
        }
        if needs_hash {
            let hash_input = if card.body.is_empty() {
                card.effective_summary()
            } else {
                card.body.as_str()
            };
            card.content_hash = Some(hash_hex(hash_input));
        }
        card.updated_at = models::utcnow();
        repository.add_knowledge_card(&card)?;
        migrated = migrated.saturating_add(1);
    }
    Ok(migrated)
}

fn hash_hex(text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn read_meta(connection: &Connection, key: &str) -> Result<Option<String>, StorageError> {
    ensure_knowledge_fts(connection)?;
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM knowledge_index_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(query_error("knowledge_index_meta"))?;
    Ok(value)
}

fn write_meta(
    connection: &Connection,
    entries: &[(&str, Option<&str>)],
) -> Result<(), StorageError> {
    ensure_knowledge_fts(connection)?;
    for (key, value) in entries {
        match value {
            Some(value) => {
                connection
                    .execute(
                        "INSERT INTO knowledge_index_meta (key, value) VALUES (?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        rusqlite::params![key, value],
                    )
                    .map_err(write_error("knowledge_index_meta(write)"))?;
            }
            None => {
                connection
                    .execute("DELETE FROM knowledge_index_meta WHERE key = ?1", [key])
                    .map_err(write_error("knowledge_index_meta(delete)"))?;
            }
        }
    }
    Ok(())
}

fn now_iso() -> String {
    // 与 payload 内 `updated_at` 同为 wire 'Z' 格式，保证 corpus_status 的
    // 字符串比较两种格式不混用（存储层时间戳编码约定）。
    serde_json::to_string(&models::utcnow())
        .ok()
        .map(|json| json.trim_matches('"').to_string())
        .unwrap_or_default()
}

fn write_error(table: &'static str) -> impl Fn(rusqlite::Error) -> StorageError {
    move |source| StorageError::Schema {
        table: table.to_string(),
        source,
    }
}

fn query_error(table: &'static str) -> impl Fn(rusqlite::Error) -> StorageError {
    move |source| StorageError::Query {
        table: table.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use models::KnowledgeCardKind;

    fn card(id: &str, title: &str, summary: &str, body: &str) -> KnowledgeCard {
        serde_json::from_value(serde_json::json!({
            "kind": "tool_usage", "title": title,
            "summary": summary, "body": body, "content": summary,
        }))
        .map(|mut card: KnowledgeCard| {
            card.id = models::KnowledgeCardId::new(id.to_string());
            card
        })
        .unwrap()
    }

    #[test]
    fn fts_roundtrip_and_bm25_ranking() {
        let connection = Connection::open_in_memory().unwrap();
        schema_guard_tables(&connection);
        let registry = AliasRegistry::embedded();
        let sqlmap = card(
            "k1",
            "sqlmap usage",
            "automated SQL injection",
            "sqlmap -u URL",
        );
        let upload = card(
            "k2",
            "文件上传利用",
            "任意文件上传 getshell",
            "upload payload",
        );
        for prepared in [&sqlmap, &upload] {
            upsert_card(&connection, prepared, registry).unwrap();
        }
        let hits = search(&connection, "sql injection", 10, registry).unwrap();
        assert_eq!(hits.first().map(|(id, _)| id.as_str()), Some("k1"));
        assert!(!hits.iter().any(|(id, _)| id == "k2"), "无关卡不命中");

        let cjk_hits = search(&connection, "文件上传", 10, registry).unwrap();
        assert_eq!(
            cjk_hits.first().map(|(id, _)| id.as_str()),
            Some("k2"),
            "中文整段词命中"
        );

        let bigram_hits = search(&connection, "文件", 10, registry).unwrap();
        assert!(
            bigram_hits.iter().any(|(id, _)| id == "k2"),
            "query 侧二元组扩出 '文件' 命中索引侧二元组"
        );
    }

    #[test]
    fn upsert_replaces_and_remove_clears() {
        let connection = Connection::open_in_memory().unwrap();
        let registry = AliasRegistry::embedded();
        let mut item = card("k1", "old title", "old", "");
        upsert_card(&connection, &item, registry).unwrap();
        item.title = "new title".to_string();
        upsert_card(&connection, &item, registry).unwrap();
        let hits = search(&connection, "new title", 10, registry).unwrap();
        assert_eq!(hits.len(), 1, "upsert 不产生重复行");
        remove_card(&connection, "k1").unwrap();
        assert!(
            search(&connection, "new title", 10, registry)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn corpus_status_transitions() {
        let connection = Connection::open_in_memory().unwrap();
        schema_guard_tables(&connection);
        let registry = AliasRegistry::embedded();
        let status = corpus_status(&connection).unwrap();
        assert_eq!(status.state, KnowledgeCorpusState::Empty);

        let item = card("k1", "title one", "summary one", "body one");
        let kind = KnowledgeCardKind::ToolUsage;
        insert_guard_card(&connection, &item, kind);
        // 未同步 → stale（卡片有，索引空）。
        let status = corpus_status(&connection).unwrap();
        assert_eq!(status.state, KnowledgeCorpusState::Stale);

        rebuild(&connection, std::slice::from_ref(&item), registry).unwrap();
        let status = corpus_status(&connection).unwrap();
        assert_eq!(status.state, KnowledgeCorpusState::Ready);
        assert_eq!(status.indexed_count, 1);
        assert!(status.last_synced_at.is_some());

        // 语料再变 → stale。
        insert_guard_card(
            &connection,
            &card("k2", "title two", "summary two", "body two"),
            kind,
        );
        let status = corpus_status(&connection).unwrap();
        assert_eq!(status.state, KnowledgeCorpusState::Stale);
    }

    // —— 守护辅助：不依赖 Repository trait，直接摆布泛型表 ——

    fn schema_guard_tables(connection: &Connection) {
        connection
            .execute(
                crate::schema::generic_table_ddl("knowledge_cards").as_str(),
                [],
            )
            .unwrap();
    }

    fn insert_guard_card(connection: &Connection, card: &KnowledgeCard, _kind: KnowledgeCardKind) {
        let payload = serde_json::to_string(card).unwrap();
        connection
            .execute(
                "INSERT INTO knowledge_cards (id, payload) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
                rusqlite::params![card.id.as_str(), payload],
            )
            .unwrap();
    }
}
