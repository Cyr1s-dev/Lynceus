#![allow(clippy::unwrap_used, clippy::expect_used, clippy::doc_markdown)]

//! 知识卡 legacy 迁移测试（§39）：旧 payload → 迁移 → FTS → 中文检索。
//!
//! 全程 deterministic（无 LLM）：旧 wire JSON 直接落库，打开仓储后跑
//! `migrate_legacy_cards`，验证 summary/body 拆分、search_terms 派生、
//! content_hash 补齐、FTS 可检索（含中文 query），且迁移幂等。

use models::KnowledgeRetrievalQuery;
use storage::{Repository, SqliteRepository, migrate_legacy_cards};

/// 打开临时库并直接插入两条 legacy wire 卡（无新字段，等价老版本写入）。
fn setup_legacy_db() -> (tempfile::TempDir, SqliteRepository) {
    let dir = tempfile::TempDir::with_prefix("knowledge_migrate_").expect("临时目录必须可创建");
    let path = dir.path().join("legacy.sqlite3");
    let repo = SqliteRepository::open(&path).expect("库必须可打开");
    let connection = rusqlite::Connection::open(&path).expect("重连必须成功");
    let legacy_rows = [
        // 旧卡 1：英文 content。
        r#"{"id":"kcard_legacy_en","kind":"tool_usage","title":"nmap service scan",
            "content":"nmap -sV service version detection baseline","tags":["recon"],
            "priority":60,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
        // 旧卡 2：中文 content（修复前 ASCII tokenizer 检索不到的内容）。
        r#"{"id":"kcard_legacy_zh","kind":"payload_strategy","title":"文件上传利用",
            "content":"任意文件上传绕过扩展名校验后上传 webshell","tags":["web"],
            "priority":70,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    ];
    for (index, row) in legacy_rows.iter().enumerate() {
        let id = if index == 0 {
            "kcard_legacy_en"
        } else {
            "kcard_legacy_zh"
        };
        connection
            .execute(
                "INSERT INTO knowledge_cards (id, payload) VALUES (?1, ?2)",
                rusqlite::params![id, row],
            )
            .expect("legacy 行必须可插入");
    }
    drop(connection);
    (dir, repo)
}

#[test]
fn legacy_cards_migrate_and_become_retrievable() {
    let (_dir, repo) = setup_legacy_db();

    // 迁移前：结构化检索（FTS）查不到中文内容——旧索引/无索引状态下
    // 检索不报错但中文词不命中英文卡之外的任何卡。
    let mut query = KnowledgeRetrievalQuery::new();
    query.text = "文件上传".to_string();
    let before = repo
        .search_knowledge_cards(&query)
        .expect("迁移前检索不得报错");
    assert!(
        !before
            .iter()
            .any(|result| result.card.id.as_str() == "kcard_legacy_zh"),
        "迁移前中文卡不得通过 FTS 命中"
    );

    // 迁移：summary/body 拆分 + search_terms 派生 + content_hash 补齐。
    let migrated = migrate_legacy_cards(&repo).expect("迁移必须成功");
    assert_eq!(migrated, 2, "两条 legacy 卡都需迁移");

    // 迁移后字段验证。
    let cards = repo.list_knowledge_cards().expect("列表必须成功");
    let zh = cards
        .iter()
        .find(|card| card.id.as_str() == "kcard_legacy_zh")
        .expect("中文卡必须存在");
    assert_eq!(zh.summary, zh.content, "content 恒等镜像 summary");
    assert_eq!(zh.body, zh.content, "body 取完整旧内容");
    assert!(zh.content_hash.is_some(), "content_hash 补齐");
    assert!(
        zh.search_terms.iter().any(|term| term == "上传"),
        "search_terms 必须含中文二元组"
    );
    let en = cards
        .iter()
        .find(|card| card.id.as_str() == "kcard_legacy_en")
        .expect("英文卡必须存在");
    assert!(en.search_terms.iter().any(|term| term == "nmap"));

    // 迁移后：中文 query 命中中文卡（§11 的核心修复）。
    let after = repo
        .search_knowledge_cards(&query)
        .expect("迁移后检索必须成功");
    assert!(
        after
            .iter()
            .any(|result| result.card.id.as_str() == "kcard_legacy_zh"),
        "迁移后中文 query 必须命中中文卡"
    );

    // 英文 query 也走 FTS 命中英文卡。
    let mut en_query = KnowledgeRetrievalQuery::new();
    en_query.text = "service version detection".to_string();
    let en_results = repo
        .search_knowledge_cards(&en_query)
        .expect("检索必须成功");
    assert!(
        en_results
            .iter()
            .any(|result| result.card.id.as_str() == "kcard_legacy_en"),
        "迁移后英文 query 必须命中英文卡"
    );

    // 语料状态：迁移后索引随写维护，但同步点落后（updated_at 变更）→
    // stale；显式 index-sync 后 ready。
    let status = repo.knowledge_corpus_status().expect("状态必须可读");
    assert_eq!(
        status.state,
        models::KnowledgeCorpusState::Stale,
        "迁移触碰语料后必须判定 stale"
    );
    let synced = repo.sync_knowledge_index().expect("同步必须成功");
    assert_eq!(synced.state, models::KnowledgeCorpusState::Ready);
    assert_eq!(synced.indexed_count, 2);

    // 幂等：再跑一次迁移 = 0 行变化。
    let again = migrate_legacy_cards(&repo).expect("二次迁移必须成功");
    assert_eq!(again, 0, "迁移必须幂等");
}
