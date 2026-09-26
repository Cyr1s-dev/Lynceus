//! 结构化安全 Wiki → Lynceus `KnowledgeUnit` 导入器。
//!
//! 数据源：知识资产目录下的 payload、工具命令和 shell 命令数据。
//!
//! 转换原则：
//! * **1 payload ≈ 1 primary strategy unit**（`payload_strategy`）；超长
//!   attackChain（> [`ATTACK_CHAIN_CHILD_THRESHOLD`] 字符）拆为
//!   `case_reference` 子单元并保留 `parent_id`；
//! * **1 tool = Tool Summary Unit + N 条独立可检索 Command Unit**
//!   （绝不出现 420KB 单卡）；
//! * **1 reverse shell command = 1 knowledge unit**，meta 映射到
//!   platform/protocol；
//! * navigation 两棵树不生成卡片——分类路径全部转为 tags；
//! * **deterministic + idempotent**：id 由 source+slug/hash 生成
//!   （`kcard_security_wiki_<src>_<slug>`），`content_hash` 变化才重写，重复
//!   导入 = 0 写；
//! * `search_terms` 经 [`storage::derive_search_terms`] 派生
//!   （别名展开 + CJK 二元组），双语内容全部保留在 summary/body。

use std::collections::HashMap;
use std::path::Path;

use models::{KnowledgeCard, KnowledgeCardDraft, KnowledgeCardKind};
use serde_json::Value;

use crate::error::ImportError;

/// attackChain 超过该字符数拆子单元。
pub const ATTACK_CHAIN_CHILD_THRESHOLD: usize = 1_800;
/// summary 组装的字符预算（对齐注入软上限）。
pub const SUMMARY_BUDGET: usize = 2_000;

type Converter = fn(&[Value]) -> Vec<KnowledgeCardDraft>;

/// 导入结果统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportSummary {
    /// 处理的源文件数。
    pub files: usize,
    /// payload 主单元数。
    pub payload_units: usize,
    /// payload attackChain 子单元数。
    pub payload_children: usize,
    /// tool summary 单元数。
    pub tool_units: usize,
    /// tool command 子单元数。
    pub command_units: usize,
    /// reverse shell 单元数。
    pub shell_units: usize,
    /// 内容未变化跳过的行数（幂等）。
    pub skipped_unchanged: usize,
    /// 解析失败跳过的条目数。
    pub skipped_invalid: usize,
}

/// 读取目录内可识别的 Wiki JSON 并导入仓储（不重建索引；调用方收口时
/// 调用 `Repository::sync_knowledge_index`）。
///
/// # Errors
/// JSON 顶层形态非法或仓储写入失败。缺失文件跳过；单条目字段缺失按
/// `skipped_invalid` 计数跳过，不中断导入。
pub fn import_directory(
    repository: &dyn storage::Repository,
    directory: &Path,
) -> Result<ImportSummary, ImportError> {
    let mut summary = ImportSummary::default();
    let existing: HashMap<String, String> = repository
        .list_knowledge_cards()?
        .into_iter()
        .filter_map(|card: KnowledgeCard| {
            card.content_hash
                .map(|hash| (card.id.as_str().to_string(), hash))
        })
        .collect();

    let files: [(&[&str], &str, Converter); 4] = [
        (&["webPayloads.json"], "webPayloads", payloads_to_units),
        (
            &["intranetPayloads.json"],
            "intranetPayloads",
            payloads_to_units,
        ),
        (
            &["toolCommands.json", "toolCommands.json.asset"],
            "toolCommands",
            tools_to_units,
        ),
        (
            &["reverseShell.json", "reverseShell.json.asset"],
            "reverseShell",
            shells_to_units,
        ),
    ];
    for (candidates, source_name, converter) in files {
        let Some(file) = candidates
            .iter()
            .copied()
            .find(|candidate| directory.join(candidate).is_file())
        else {
            continue;
        };
        let path = directory.join(file);
        let raw = std::fs::read_to_string(&path).map_err(|source| ImportError::SourceFile {
            path: path.display().to_string(),
            source,
        })?;
        let parsed: Value =
            serde_json::from_str(&raw).map_err(|error| ImportError::InvalidSource {
                file: file.to_string(),
                detail: error.to_string(),
            })?;
        let mut items: Vec<Value> = match parsed {
            Value::Array(items) => items,
            Value::Object(object) => {
                // reverseShell.json 是 {cmd:[], bind:[], hoax:[], msf:[]} 分组对象。
                let mut items = Vec::new();
                for (category, group) in object {
                    if let Value::Array(entries) = group {
                        for mut entry in entries {
                            if let Value::Object(map) = &mut entry {
                                map.entry("__category".to_string())
                                    .or_insert_with(|| Value::String(category.clone()));
                            }
                            items.push(entry);
                        }
                    }
                }
                items
            }
            _ => {
                return Err(ImportError::InvalidSource {
                    file: file.to_string(),
                    detail: "top level must be array or grouped object".to_string(),
                });
            }
        };
        // payload 文件注入来源（intranet 与 web 的 platform/locator 依赖它）。
        let stem = source_name.to_string();
        for item in &mut items {
            if let Value::Object(map) = item {
                map.entry("__source".to_string())
                    .or_insert_with(|| Value::String(stem.clone()));
            }
        }
        summary.files += 1;
        for draft in converter(&items) {
            let Some(hash) = content_hash_of(&draft) else {
                summary.skipped_invalid += 1;
                continue;
            };
            if existing
                .get(draft_id(&draft))
                .is_some_and(|existing| *existing == hash)
            {
                summary.skipped_unchanged += 1;
                continue;
            }
            let card = finish_draft(draft);
            count_unit(&mut summary, &card);
            repository.add_knowledge_card(&card)?;
        }
    }
    Ok(summary)
}

/// 按来源/kind 归类计数（幂等重跑 = 全部 skip，计数不变）。
fn count_unit(summary: &mut ImportSummary, card: &KnowledgeCard) {
    let source = card.source.as_deref().unwrap_or("");
    if source.contains("reverseShell") {
        summary.shell_units += 1;
    } else if source.contains("toolCommands") {
        if card.parent_id.is_some() {
            summary.command_units += 1;
        } else {
            summary.tool_units += 1;
        }
    } else if card.kind == KnowledgeCardKind::CaseReference {
        summary.payload_children += 1;
    } else {
        summary.payload_units += 1;
    }
}

/// Wiki JSON 条目 → 草稿卡片的转换函数签名。
/// payload 的 summary 文本（description + 前 3 条命令 + OPSEC）。
fn payload_summary_text(description: &str, execution: &[Value], opsec: &str) -> String {
    let mut summary = String::new();
    push_line(&mut summary, description);
    for step in execution.iter().take(3) {
        let command = step.get("command").and_then(Value::as_str).unwrap_or("");
        if !command.is_empty() {
            push_line(&mut summary, &format!("> {}", truncate(command, 160)));
        }
    }
    if !opsec.is_empty() {
        push_line(&mut summary, &format!("OPSEC: {opsec}"));
    }
    summary
}

/// payload 的完整 body 文本（结构化步骤 + 分析/WAF/OPSEC/教程）。
fn payload_body_text(
    name: &str,
    description: &str,
    category: &str,
    sub_category: &str,
    item: &Value,
    execution: &[Value],
    opsec: &str,
) -> String {
    let mut body = String::new();
    push_line(&mut body, &format!("# {name}"));
    push_line(&mut body, &format!("分类: {category} / {sub_category}"));
    push_line(&mut body, &format!("描述: {description}"));
    if let Some(prerequisites) = bilingual_array(item, "prerequisites") {
        push_line(&mut body, &format!("前置: {prerequisites}"));
    }
    for (index, step) in execution.iter().enumerate() {
        let title = bilingual(step.get("title"));
        let command = step.get("command").and_then(Value::as_str).unwrap_or("");
        let note = bilingual(step.get("description"));
        push_line(&mut body, &format!("步骤{}: {title}", index + 1));
        if !command.is_empty() {
            push_line(&mut body, &format!("命令: {command}"));
        }
        if !note.is_empty() {
            push_line(&mut body, &format!("说明: {note}"));
        }
    }
    let analysis = bilingual_field(item, "analysis");
    if !analysis.is_empty() {
        push_line(&mut body, &format!("结果分析: {analysis}"));
    }
    let waf = bilingual_field(item, "wafBypass");
    if !waf.is_empty() {
        push_line(&mut body, &format!("WAF bypass: {waf}"));
    }
    if !opsec.is_empty() {
        push_line(&mut body, &format!("OPSEC: {opsec}"));
    }
    let tutorial = bilingual_field(item, "tutorial");
    if !tutorial.is_empty() {
        push_line(&mut body, &format!("教程: {tutorial}"));
    }
    body
}

/// 转换 payload 条目（web/intranet 共用；文件来源经 `__source` 注入）。
#[must_use]
pub fn payloads_to_units(items: &[Value]) -> Vec<KnowledgeCardDraft> {
    let mut units = Vec::with_capacity(items.len());
    for item in items {
        let Some(identifier) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = bilingual(item.get("name"));
        let description = bilingual(item.get("description"));
        let category = bilingual(item.get("category"));
        let sub_category = bilingual(item.get("subCategory"));
        let source = format!(
            "security-wiki:{}:{}",
            item.get("__source")
                .and_then(Value::as_str)
                .unwrap_or("webPayloads"),
            identifier
        );
        let tags = collect_tags(item, &[&category, &sub_category]);
        let execution = item
            .get("execution")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let opsec = bilingual_field(item, "opsecTips");
        let summary = truncate(
            &payload_summary_text(&description, &execution, &opsec),
            SUMMARY_BUDGET,
        );
        let body = payload_body_text(
            &name,
            &description,
            &category,
            &sub_category,
            item,
            &execution,
            &opsec,
        );
        let parent_id = models::KnowledgeCardId::new(unit_id(&source, identifier));
        units.push(KnowledgeCardDraft {
            id: Some(parent_id.clone()),
            kind: KnowledgeCardKind::PayloadStrategy,
            title: truncate(&name, 160),
            summary,
            body,
            parent_id: None,
            aliases: vec![],
            tags,
            tool: extract_tool(item),
            technique: vec![slug_of(&sub_category)],
            platform: platform_hints(&source),
            protocol: vec![],
            prerequisites: vec![],
            source: Some(truncate(&source, 300)),
            source_locator: Some(truncate(identifier, 300)),
            priority: 65,
        });
        // 超长 attackChain → case_reference 子单元。
        let chain = bilingual_field(item, "attackChain");
        if chain.chars().count() > ATTACK_CHAIN_CHILD_THRESHOLD {
            units.push(KnowledgeCardDraft {
                id: Some(models::KnowledgeCardId::new(format!("{parent_id}:chain"))),
                kind: KnowledgeCardKind::CaseReference,
                title: truncate(&format!("{name} - 攻击链"), 160),
                summary: truncate(&chain, SUMMARY_BUDGET),
                body: chain,
                parent_id: Some(parent_id),
                aliases: vec![],
                tags: vec!["attack-chain".to_string()],
                tool: vec![],
                technique: vec![],
                platform: vec![],
                protocol: vec![],
                prerequisites: vec![],
                source: None,
                source_locator: None,
                priority: 55,
            });
        }
    }
    units
}

/// 转换 toolCommands：1 tool = summary unit + N command units。
#[must_use]
pub fn tools_to_units(items: &[Value]) -> Vec<KnowledgeCardDraft> {
    let mut units = Vec::with_capacity(items.len() * 4);
    for item in items {
        let Some(identifier) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = bilingual(item.get("name"));
        let description = bilingual(item.get("description"));
        let category = bilingual(item.get("category"));
        let installation = bilingual_field(item, "installation");
        let source = format!("security-wiki:toolCommands:{identifier}");
        let parent_id = models::KnowledgeCardId::new(unit_id(&source, identifier));
        let mut summary = String::new();
        push_line(&mut summary, &description);
        if !installation.is_empty() {
            push_line(&mut summary, &format!("安装: {installation}"));
        }
        let commands = item
            .get("commands")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for command in commands.iter().take(4) {
            let command_text = command.get("command").and_then(Value::as_str).unwrap_or("");
            if !command_text.is_empty() {
                push_line(&mut summary, &format!("> {}", truncate(command_text, 140)));
            }
        }
        units.push(KnowledgeCardDraft {
            id: Some(parent_id.clone()),
            kind: KnowledgeCardKind::ToolUsage,
            title: truncate(&name, 160),
            summary: truncate(&summary, SUMMARY_BUDGET),
            body: format!(
                "# {name}\n分类: {category}\n描述: {description}\n安装: {installation}\n命令数: {}",
                commands.len()
            ),
            parent_id: None,
            aliases: vec![],
            tags: collect_tags(item, &[&category]),
            tool: vec![slug_of(identifier)],
            technique: vec![slug_of(&category)],
            platform: vec![],
            protocol: vec![],
            prerequisites: vec![],
            source: Some(truncate(&source, 300)),
            source_locator: Some(truncate(identifier, 300)),
            priority: 60,
        });
        for command in &commands {
            let Some(command_text) = command.get("command").and_then(Value::as_str) else {
                continue;
            };
            if command_text.trim().is_empty() {
                continue;
            }
            let command_name = bilingual(command.get("name"));
            let note = bilingual(command.get("description"));
            let platform = command
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut body = format!("命令: {command_text}\n说明: {note}\n平台: {platform}");
            if let Some(breakdown) = command.get("syntaxBreakdown").and_then(Value::as_array) {
                for part in breakdown {
                    let piece = part.get("part").and_then(Value::as_str).unwrap_or("");
                    let explanation = bilingual(part.get("explanation"));
                    if !piece.is_empty() {
                        push_line(&mut body, &format!("语法 {piece}: {explanation}"));
                    }
                }
            }
            units.push(KnowledgeCardDraft {
                id: Some(models::KnowledgeCardId::new(format!(
                    "{parent_id}:cmd:{}",
                    short_hash(command_text)
                ))),
                kind: KnowledgeCardKind::ToolUsage,
                title: truncate(&format!("{command_name} ({identifier})"), 160),
                summary: truncate(&format!("{command_name}: {note}"), SUMMARY_BUDGET),
                body,
                parent_id: Some(parent_id.clone()),
                aliases: vec![],
                tags: vec![slug_of(identifier)],
                tool: vec![slug_of(identifier)],
                technique: vec![],
                platform: platform_hints(platform),
                protocol: vec![],
                prerequisites: vec![],
                source: Some(truncate(&source, 300)),
                source_locator: Some(truncate(&format!("{identifier}#command"), 300)),
                priority: 55,
            });
        }
    }
    units
}

/// 转换 reverseShell：1 command = 1 unit，meta → platform。
#[must_use]
pub fn shells_to_units(items: &[Value]) -> Vec<KnowledgeCardDraft> {
    let mut units = Vec::with_capacity(items.len());
    for item in items {
        let Some(command) = item.get("command").and_then(Value::as_str) else {
            continue;
        };
        if command.trim().is_empty() {
            continue;
        }
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let category = item
            .get("__category")
            .and_then(Value::as_str)
            .unwrap_or("cmd");
        let mut meta_parts = Vec::new();
        if let Some(meta) = item.get("meta").and_then(Value::as_array) {
            for entry in meta {
                if let Some(text) = entry.as_str() {
                    meta_parts.push(text.to_string());
                }
            }
        }
        let meta = meta_parts.join(" ");
        let source = format!("security-wiki:reverseShell:{category}");
        let identifier = format!("{category}:{}", short_hash(&format!("{name}|{command}")));
        units.push(KnowledgeCardDraft {
            id: Some(models::KnowledgeCardId::new(unit_id(&source, &identifier))),
            kind: KnowledgeCardKind::ToolUsage,
            title: truncate(&format!("{name} ({category})"), 160),
            summary: truncate(&format!("{name}: {meta}"), SUMMARY_BUDGET),
            body: format!("命令: {command}\n说明: {meta}\n类别: {category}"),
            parent_id: None,
            aliases: vec!["reverse shell".to_string()],
            tags: vec!["shell".to_string(), category.to_string()],
            tool: vec![category.to_string()],
            technique: vec![],
            platform: platform_hints(&format!("{category} {meta}")),
            protocol: vec![],
            prerequisites: vec![],
            source: Some(truncate(&source, 300)),
            source_locator: Some(truncate(&identifier, 300)),
            priority: 55,
        });
    }
    units
}

/// 组装最终卡片（summary 回退 + `search_terms` 派生 + `content_hash`）。
#[must_use]
pub fn finish_draft(mut draft: KnowledgeCardDraft) -> KnowledgeCard {
    if draft.summary.trim().is_empty() {
        draft.summary = truncate(&draft.body, SUMMARY_BUDGET);
    }
    if draft.summary.trim().is_empty() {
        draft.summary = draft.title.clone();
    }
    let content_hash = draft_content_hash(&draft);
    let aliases = draft.aliases.join(" ");
    let tags = draft.tags.join(" ");
    let tool = draft.tool.join(" ");
    let technique = draft.technique.join(" ");
    let search_terms = storage::derive_search_terms(
        &[
            &draft.title,
            &draft.summary,
            &draft.body,
            &aliases,
            &tags,
            &tool,
            &technique,
        ],
        storage::AliasRegistry::embedded(),
    );
    let summary = draft.summary.clone();
    KnowledgeCard {
        id: draft
            .id
            .unwrap_or_else(|| models::KnowledgeCardId::new(models::new_id("kcard"))),
        kind: draft.kind,
        title: draft.title,
        summary: summary.clone(),
        body: draft.body,
        content: summary,
        parent_id: draft.parent_id,
        aliases: draft.aliases,
        tags: draft.tags,
        tool: draft.tool,
        technique: draft.technique,
        platform: draft.platform,
        protocol: draft.protocol,
        prerequisites: draft.prerequisites,
        source: draft.source,
        source_locator: draft.source_locator,
        content_hash: Some(content_hash),
        search_terms,
        priority: draft.priority,
        embedding_version: None,
        created_at: models::utcnow(),
        updated_at: models::utcnow(),
    }
}

/// 幂等键：内容哈希（title+summary+body）；缺关键字段返回 `None`。
#[must_use]
pub fn content_hash_of(draft: &KnowledgeCardDraft) -> Option<String> {
    if draft.title.trim().is_empty() || draft.summary.trim().is_empty() {
        return None;
    }
    Some(draft_content_hash(draft))
}

fn draft_id(draft: &KnowledgeCardDraft) -> &str {
    draft
        .id
        .as_ref()
        .map_or("kcard_unspecified", models::KnowledgeCardId::as_str)
}

fn draft_content_hash(draft: &KnowledgeCardDraft) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(
        format!("{}\x00{}\x00{}", draft.title, draft.summary, draft.body).as_bytes(),
    );
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 短哈希（8 hex），用于无自然 id 的条目。
#[must_use]
pub fn short_hash(text: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

/// 确定性单元 id：`kcard_security_wiki_<src>_<slug>`。
fn unit_id(source: &str, identifier: &str) -> String {
    format!(
        "kcard_security_wiki_{}_{}",
        slug_of(source.split(':').nth(1).unwrap_or("misc")),
        slug_of(identifier)
    )
}

/// slug 化：仅保留 ASCII 字母数字与 `-`，其余折叠为 `-`。
#[must_use]
pub fn slug_of(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

/// `{zh, en}` 双语字段 → `zh | en`。
#[must_use]
pub fn bilingual(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let zh = value.get("zh").and_then(Value::as_str).unwrap_or("").trim();
    let en = value.get("en").and_then(Value::as_str).unwrap_or("").trim();
    match (zh.is_empty(), en.is_empty()) {
        (false, false) => format!("{zh} | {en}"),
        (false, true) => zh.to_string(),
        (true, false) => en.to_string(),
        (true, true) => String::new(),
    }
}

/// 顶层双语字段便捷读取。
fn bilingual_field(item: &Value, key: &str) -> String {
    bilingual(item.get(key))
}

/// 双语数组字段拼接。
fn bilingual_array(item: &Value, key: &str) -> Option<String> {
    let items = item.get(key)?.as_array()?;
    let parts: Vec<String> = items
        .iter()
        .map(|value| bilingual(Some(value)))
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("；"))
    }
}

/// tags 数组 + 分类路径 → 归一化 tags（≤16，slug 化 ASCII 优先）。
fn collect_tags(item: &Value, category_paths: &[&String]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let push = |tag: &str, tags: &mut Vec<String>| {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() || tags.contains(&tag) {
            return;
        }
        if tags.len() < 16 {
            tags.push(tag);
        }
    };
    if let Some(list) = item.get("tags").and_then(Value::as_array) {
        for tag in list {
            if let Some(text) = tag.as_str() {
                push(text, &mut tags);
            }
        }
    }
    for path in category_paths {
        for segment in path.split('/') {
            push(&slug_of(segment), &mut tags);
        }
    }
    tags
}

/// 平台提示（payload 文件来源 / 命令平台 / shell 类别）。
fn platform_hints(source: &str) -> Vec<String> {
    let lower = source.to_lowercase();
    if lower.contains("intranet") {
        vec![
            "windows".to_string(),
            "linux".to_string(),
            "intranet".to_string(),
        ]
    } else if lower.contains("webpayloads") {
        vec!["web".to_string()]
    } else if lower.contains("windows") {
        vec!["windows".to_string()]
    } else if lower.contains("linux") {
        vec!["linux".to_string()]
    } else {
        Vec::new()
    }
}

/// 从 payload 的 execution 命令里启发式提取工具名。
fn extract_tool(item: &Value) -> Vec<String> {
    let mut tools = Vec::new();
    if let Some(execution) = item.get("execution").and_then(Value::as_array) {
        for step in execution.iter().take(5) {
            let command = step.get("command").and_then(Value::as_str).unwrap_or("");
            if let Some(first) = command.split_whitespace().next() {
                let slug = slug_of(first);
                if !slug.is_empty() && !tools.contains(&slug) && tools.len() < 4 {
                    tools.push(slug);
                }
            }
        }
    }
    tools
}

fn push_line(buffer: &mut String, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(line);
}

/// 字符级截断（中文安全），超限加省略号。
#[must_use]
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

pub mod error;
#[cfg(test)]
mod tests;
