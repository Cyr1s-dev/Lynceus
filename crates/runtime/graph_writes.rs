//! 手工图写入 —— `server/core/engine/manager.py` 的用户驱动节点入口。
//!
//! API/Tauri 等控制面只能通过这里追加 Fact、Intent、Hint、Evidence 与
//! Finding。这样引用校验、数值边界与后续审计钩子有单一落点，仓储仍只
//! 负责持久化，不暴露成任意写接口。

#![allow(clippy::doc_markdown)]

use std::collections::{BTreeSet, HashMap};

use models::{
    AuditEventType, Evidence, EvidenceKind, Fact, Finding, FindingStatus, Hint, Intent,
    KnowledgeCard, KnowledgeCardDraft, KnowledgeCardKind, KnowledgeCorpusStatus,
    KnowledgeRetrievalQuery, KnowledgeRetrievalResult, ProjectId, Severity,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::manager::AuditManager;

fn normalized_finding_label(value: Option<&str>) -> String {
    value
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 知识卡文本字段边界校验（`min=0` 允许空串）。
fn validate_card_text(field: &str, value: &str, min: usize, max: usize) -> Result<(), EngineError> {
    let count = value.trim().chars().count();
    if count < min || count > max {
        return Err(EngineError::Value(format!(
            "knowledge card {field} must contain {min}-{max} characters"
        )));
    }
    Ok(())
}

/// 词条类字段归一化（strip + 小写 + 去重保序）。
fn normalize_terms(terms: Vec<String>, max_items: usize) -> Result<Vec<String>, EngineError> {
    let mut normalized = Vec::new();
    for term in terms {
        let term = term.trim().to_lowercase();
        if !term.is_empty() && !normalized.contains(&term) {
            normalized.push(term);
        }
    }
    if normalized.len() > max_items {
        return Err(EngineError::Value(format!(
            "knowledge card term list must contain at most {max_items} items"
        )));
    }
    Ok(normalized)
}

/// 知识内容 SHA-256（hex；body 优先，缺省 summary）。
fn card_content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn finding_fingerprint(finding: &Finding, evidence_by_id: &HashMap<String, Evidence>) -> String {
    let mut anchors = BTreeSet::new();
    let mut evidence_ids = finding.evidence_ids.clone();
    evidence_ids.sort_unstable();
    for evidence_id in evidence_ids {
        let Some(evidence) = evidence_by_id.get(&evidence_id) else {
            continue;
        };
        for location in &evidence.locations {
            anchors.insert(format!(
                "{}:{}:{}:{}:{}",
                normalized_finding_label(Some(&location.artifact)).replace('\\', "/"),
                location
                    .start_line
                    .map_or_else(String::new, |value| value.to_string()),
                location
                    .end_line
                    .map_or_else(String::new, |value| value.to_string()),
                normalized_finding_label(location.symbol.as_deref()),
                normalized_finding_label(location.address.as_deref()),
            ));
        }
    }
    if anchors.is_empty() {
        anchors.insert(format!(
            "unlocated:{}",
            normalized_finding_label(Some(&finding.title))
        ));
    }

    let rule_id = normalized_finding_label(finding.rule_id.as_deref());
    let weakness = finding
        .cwe
        .as_deref()
        .map(|value| normalized_finding_label(Some(value)))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| rule_id.clone());
    let mut sink_label = normalized_finding_label(finding.sink_label.as_deref());
    if sink_label == rule_id || rule_id.ends_with(&format!(":{sink_label}")) {
        sink_label.clear();
    }
    let mut parts = vec![
        "finding-v2".to_string(),
        normalized_finding_label(Some(finding.project_id.as_str())),
        weakness,
        normalized_finding_label(finding.source_label.as_deref()),
        sink_label,
    ];
    parts.extend(anchors);
    let digest = Sha256::digest(parts.join("|").as_bytes());
    format!("v2:{digest:x}")[..27].to_string()
}

impl AuditManager {
    /// 写入紧凑知识卡（legacy 入口，`content` 即 `summary`），供策略板和
    /// 检索器消费。
    ///
    /// # Errors
    /// 字段超出契约边界或仓储写入失败。
    pub fn add_knowledge_card(
        &self,
        kind: KnowledgeCardKind,
        title: &str,
        content: &str,
        tags: Vec<String>,
        source: Option<String>,
        priority: i64,
    ) -> Result<KnowledgeCard, EngineError> {
        self.add_knowledge_unit(KnowledgeCardDraft {
            kind,
            title: title.to_string(),
            summary: content.to_string(),
            tags,
            source,
            priority,
            ..KnowledgeCardDraft::default()
        })
    }

    /// 写入完整知识单元（Retrieval Substrate 入口）。
    ///
    /// 校验边界（与 Python 契约对齐 + Retrieval Substrate 扩展）：
    /// `title ∈ [1,160]`、`summary ∈ [1,2000]`、`body ≤ 512000`、
    /// `tags ≤ 16`、`source ≤ 300`、`source_locator ≤ 300`、词条类字段
    /// 各 `≤ 32`、`priority ∈ [0,100]`。`content` 恒等镜像 `summary`，
    /// `content_hash` 为 body（缺省 summary）的 SHA-256。
    ///
    /// # Errors
    /// 字段超出契约边界或仓储写入失败。
    pub fn add_knowledge_unit(
        &self,
        draft: KnowledgeCardDraft,
    ) -> Result<KnowledgeCard, EngineError> {
        validate_card_text("title", &draft.title, 1, 160)?;
        validate_card_text("summary", &draft.summary, 1, models::MAX_SUMMARY_CHARS)?;
        if draft.body.chars().count() > models::MAX_BODY_CHARS {
            return Err(EngineError::Value(format!(
                "knowledge body must contain at most {} characters",
                models::MAX_BODY_CHARS
            )));
        }
        if !(0..=100).contains(&draft.priority) {
            return Err(EngineError::Value(
                "knowledge card priority must be between 0 and 100".to_string(),
            ));
        }
        // Python 契约：tags ≤16；其余词条类字段 ≤32（Retrieval Substrate）。
        let normalized_tags = normalize_terms(draft.tags, 16)?;
        let aliases = normalize_terms(draft.aliases, models::MAX_STRUCTURED_TERMS)?;
        let tool = normalize_terms(draft.tool, models::MAX_STRUCTURED_TERMS)?;
        let technique = normalize_terms(draft.technique, models::MAX_STRUCTURED_TERMS)?;
        let platform = normalize_terms(draft.platform, models::MAX_STRUCTURED_TERMS)?;
        let protocol = normalize_terms(draft.protocol, models::MAX_STRUCTURED_TERMS)?;
        let prerequisites = normalize_terms(draft.prerequisites, models::MAX_STRUCTURED_TERMS)?;
        if let Some(locator) = draft.source_locator.as_ref() {
            validate_card_text(
                "source_locator",
                locator,
                0,
                models::MAX_SOURCE_LOCATOR_CHARS,
            )?;
        }
        if let Some(source) = draft.source.as_ref() {
            validate_card_text("source", source, 0, 300)?;
        }
        let now = models::utcnow();
        let hash_input = if draft.body.is_empty() {
            draft.summary.as_str()
        } else {
            draft.body.as_str()
        };
        let card = KnowledgeCard {
            id: draft
                .id
                .unwrap_or_else(|| models::KnowledgeCardId::new(models::new_id("kcard"))),
            kind: draft.kind,
            title: draft.title.trim().to_string(),
            summary: draft.summary.trim().to_string(),
            body: draft.body.clone(),
            content: draft.summary.trim().to_string(),
            parent_id: draft.parent_id,
            aliases,
            tags: normalized_tags,
            tool,
            technique,
            platform,
            protocol,
            prerequisites,
            source: draft.source,
            source_locator: draft.source_locator,
            content_hash: Some(card_content_hash(hash_input)),
            search_terms: Vec::new(),
            priority: draft.priority,
            embedding_version: None,
            created_at: now,
            updated_at: now,
        };
        Ok(self.repository().add_knowledge_card(&card)?)
    }

    /// 列出全部知识卡。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn list_knowledge_cards(&self) -> Result<Vec<KnowledgeCard>, EngineError> {
        Ok(self.repository().list_knowledge_cards()?)
    }

    /// 知识检索（QueryNormalizer + 结构化过滤 + FTS5/BM25，FTS 不可用
    /// 时确定性回退线性扫描）。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn search_knowledge_cards(
        &self,
        query: &KnowledgeRetrievalQuery,
    ) -> Result<Vec<KnowledgeRetrievalResult>, EngineError> {
        Ok(self.repository().search_knowledge_cards(query)?)
    }

    /// 全量重建知识 FTS 索引（index-sync）。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub fn sync_knowledge_index(&self) -> Result<KnowledgeCorpusStatus, EngineError> {
        Ok(self.repository().sync_knowledge_index()?)
    }

    /// 知识语料/索引状态（empty/ready/stale）。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn knowledge_corpus_status(&self) -> Result<KnowledgeCorpusStatus, EngineError> {
        Ok(self.repository().knowledge_corpus_status()?)
    }

    /// 追加用户 Fact（append-only）。
    ///
    /// # Errors
    /// Project 不存在、置信度越界或仓储写入失败。
    pub fn add_user_fact(
        &self,
        project_id: &str,
        kind: &str,
        statement: &str,
        data: Map<String, Value>,
        derived_from: Vec<String>,
        confidence: f64,
    ) -> Result<Fact, EngineError> {
        self.require_project(project_id)?;
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err(EngineError::Value(
                "confidence must be between 0 and 1".to_string(),
            ));
        }
        let mut fact = Fact::new(
            ProjectId::new(project_id.to_string()),
            kind.to_string(),
            statement.to_string(),
        );
        fact.data = data;
        fact.derived_from = derived_from;
        fact.confidence = confidence;
        Ok(self.repository().add_fact(&fact)?)
    }

    /// 追加用户 Intent（`created_by = "user"`）。
    ///
    /// # Errors
    /// Project/Fact 引用不存在、优先级越界或仓储写入失败。
    pub fn add_user_intent(
        &self,
        project_id: &str,
        title: &str,
        description: Option<String>,
        source_fact_ids: Vec<String>,
        solver: Option<String>,
        priority: i64,
    ) -> Result<Intent, EngineError> {
        self.require_project(project_id)?;
        if !(0..=100).contains(&priority) {
            return Err(EngineError::Value(
                "priority must be between 0 and 100".to_string(),
            ));
        }
        let known = self
            .repository()
            .list_facts(project_id)?
            .into_iter()
            .map(|fact| fact.id.as_str().to_string())
            .collect::<std::collections::HashSet<_>>();
        if let Some(missing) = source_fact_ids.iter().find(|id| !known.contains(*id)) {
            return Err(EngineError::ReferenceValidationError(format!(
                "source_fact_ids references unknown fact: {missing}"
            )));
        }
        let mut intent = Intent::new(ProjectId::new(project_id.to_string()), title.to_string());
        intent.description = description;
        intent.source_fact_ids = source_fact_ids;
        intent.solver = solver;
        intent.priority = priority;
        intent.created_by = "user".to_string();
        Ok(self.repository().add_intent(&intent)?)
    }

    /// 追加用户 Hint（Hint 不属于 Fact 证据链）。
    ///
    /// # Errors
    /// Project 不存在、权重越界或仓储写入失败。
    pub fn add_user_hint(
        &self,
        project_id: &str,
        text: &str,
        category: Option<String>,
        weight: i64,
    ) -> Result<Hint, EngineError> {
        self.require_project(project_id)?;
        if !(0..=100).contains(&weight) {
            return Err(EngineError::Value(
                "weight must be between 0 and 100".to_string(),
            ));
        }
        let mut hint = Hint::new(ProjectId::new(project_id.to_string()), text.to_string());
        hint.category = category;
        hint.weight = weight;
        Ok(self.repository().add_hint(&hint)?)
    }

    /// 手工追加 Evidence，并校验其支持的 Fact 均属于该 Project。
    ///
    /// # Errors
    /// Project/Fact 引用不存在或仓储写入失败。
    pub fn add_manual_evidence(
        &self,
        project_id: &str,
        kind: EvidenceKind,
        summary: &str,
        content: Map<String, Value>,
        supports_fact_ids: Vec<String>,
    ) -> Result<Evidence, EngineError> {
        self.require_project(project_id)?;
        let known = self
            .repository()
            .list_facts(project_id)?
            .into_iter()
            .map(|fact| fact.id.as_str().to_string())
            .collect::<std::collections::HashSet<_>>();
        if let Some(missing) = supports_fact_ids.iter().find(|id| !known.contains(*id)) {
            return Err(EngineError::ReferenceValidationError(format!(
                "supports_fact_ids references unknown fact: {missing}"
            )));
        }
        let mut evidence = Evidence::new(
            ProjectId::new(project_id.to_string()),
            kind,
            summary.to_string(),
        );
        evidence.content = content;
        evidence.supports_fact_ids = supports_fact_ids;
        Ok(self.repository().add_evidence(&evidence)?)
    }

    /// 人工 triage：改写 Finding 的 `status` / `severity`。
    ///
    /// 与 [`Self::add_manual_finding`] 的差异：这里允许 Operator 显式把
    /// 状态推到 `confirmed`（证据门仍由 `Finding::validated` 兜底——
    /// `confirmed` 必须有证据），也允许推到 `fixed` / `false_positive` /
    /// `duplicate` 等人工结论。状态机只校验取值合法，不限制迁移方向：
    /// 误报可以翻回 candidate，已修复也可以因回归重新打开。
    ///
    /// # Errors
    /// - [`EngineError::FindingNotFound`]：Finding 不存在。
    /// - [`EngineError::Value`]：`status` / `severity` 不是合法 wire 值。
    /// - [`EngineError::ReferenceValidationError`]：目标状态违反不变量
    ///   （如 `confirmed` 但 `evidence_ids` 为空）。
    pub async fn triage_finding(
        &self,
        finding_id: &str,
        status: Option<FindingStatus>,
        severity: Option<Severity>,
    ) -> Result<Finding, EngineError> {
        let mut finding = self
            .repository()
            .get_finding(finding_id)?
            .ok_or_else(|| EngineError::FindingNotFound(format!("unknown finding: {finding_id}")))?;
        let previous_status = finding.status;
        let previous_severity = finding.severity;
        if let Some(status) = status {
            finding.status = status;
        }
        if let Some(severity) = severity {
            finding.severity = severity;
        }
        // 走 validated()：confirmed 无证据这类不变量在写库前就被拦住。
        let finding = finding.validated().map_err(|error| {
            EngineError::ReferenceValidationError(format!(
                "finding {finding_id} cannot enter the requested state: {error}"
            ))
        })?;
        let stored = self.repository().update_finding(&finding)?;
        if previous_status != stored.status || previous_severity != stored.severity {
            let notification = crate::notifications::notification_for_finding(&stored);
            self.publish_notification(&notification);
        }
        self.record_event_safe(EventDraft {
            severity: Some(stored.severity.as_str()),
            data: Some(Map::from_iter([
                (
                    "finding_id".to_string(),
                    Value::String(stored.id.as_str().to_string()),
                ),
                (
                    "from_status".to_string(),
                    Value::String(previous_status.as_str().to_string()),
                ),
                (
                    "to_status".to_string(),
                    Value::String(stored.status.as_str().to_string()),
                ),
            ])),
            ..EventDraft::new(
                &stored.project_id,
                AuditEventType::FindingTriageUpdated,
                "user",
                &format!(
                    "Finding triaged: {} ({} → {})",
                    stored.title.chars().take(60).collect::<String>(),
                    previous_status.as_str(),
                    stored.status.as_str(),
                ),
            )
        })
        .await;
        Ok(stored)
    }

    /// 手工追加 Finding，并校验其 Evidence/Fact 引用均属于该 Project。
    ///
    /// 手工 Finding 保持 `candidate` 状态；只有 Observer/证据门完成审查后
    /// 才允许进入 `confirmed`，避免 API 直接绕过证据门。
    ///
    /// # Errors
    /// Project/节点引用不存在或仓储写入失败。
    #[allow(clippy::too_many_arguments)]
    pub async fn add_manual_finding(
        &self,
        project_id: &str,
        title: &str,
        description: Option<String>,
        severity: Severity,
        cwe: Option<String>,
        rule_id: Option<String>,
        evidence_ids: Vec<String>,
        related_fact_ids: Vec<String>,
        source_label: Option<String>,
        sink_label: Option<String>,
    ) -> Result<Finding, EngineError> {
        self.require_project(project_id)?;
        let evidence_by_id = self
            .repository()
            .list_evidence(project_id)?
            .into_iter()
            .map(|item| (item.id.as_str().to_string(), item))
            .collect::<HashMap<_, _>>();
        if let Some(missing) = evidence_ids
            .iter()
            .find(|id| !evidence_by_id.contains_key(*id))
        {
            return Err(EngineError::ReferenceValidationError(format!(
                "evidence_ids references unknown evidence: {missing}"
            )));
        }
        let known_facts = self
            .repository()
            .list_facts(project_id)?
            .into_iter()
            .map(|item| item.id.as_str().to_string())
            .collect::<std::collections::HashSet<_>>();
        if let Some(missing) = related_fact_ids
            .iter()
            .find(|id| !known_facts.contains(*id))
        {
            return Err(EngineError::ReferenceValidationError(format!(
                "related_fact_ids references unknown fact: {missing}"
            )));
        }
        let mut finding = Finding::new(ProjectId::new(project_id.to_string()), title.to_string());
        finding.description = description;
        finding.severity = severity;
        finding.cwe = cwe;
        finding.rule_id = rule_id;
        finding.evidence_ids = evidence_ids;
        finding.related_fact_ids = related_fact_ids;
        finding.source_label = source_label;
        finding.sink_label = sink_label;
        finding.fingerprint = Some(finding_fingerprint(&finding, &evidence_by_id));

        let mut existing = self.repository().list_findings(project_id)?;
        existing.sort_unstable_by_key(|item| item.created_at);
        let mut canonical_by_fingerprint = HashMap::new();
        for mut item in existing {
            if item.status == FindingStatus::Duplicate {
                continue;
            }
            let fingerprint = finding_fingerprint(&item, &evidence_by_id);
            item.fingerprint = Some(fingerprint.clone());
            canonical_by_fingerprint.entry(fingerprint).or_insert(item);
        }
        if let Some(fingerprint) = finding.fingerprint.as_ref()
            && let Some(canonical) = canonical_by_fingerprint.get(fingerprint)
        {
            finding.status = FindingStatus::Duplicate;
            finding.dedup_of = Some(canonical.id.as_str().to_string());
        }
        let stored = self.repository().add_finding(&finding)?;
        // NOTIFY 平面：发现落库即发布（Python NotifyingRepository.add_finding）。
        let notification = crate::notifications::notification_for_finding(&stored);
        self.publish_notification(&notification);
        let event_title = format!(
            "Finding added: {}",
            title.chars().take(60).collect::<String>()
        );
        let severity_label = severity.as_str();
        self.record_event_safe(EventDraft {
            severity: Some(severity_label),
            data: Some(Map::from_iter([
                (
                    "finding_id".to_string(),
                    Value::String(stored.id.as_str().to_string()),
                ),
                (
                    "severity".to_string(),
                    Value::String(severity_label.to_string()),
                ),
            ])),
            ..EventDraft::new(
                &stored.project_id,
                AuditEventType::FindingAdded,
                "user",
                &event_title,
            )
        })
        .await;
        Ok(stored)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agents::solver::SolverRegistry;
    use models::AuditDomain;

    use super::*;

    fn manager() -> AuditManager {
        let dir = tempfile::tempdir().expect("temporary directory must be available");
        let repo = storage::SqliteRepository::open(dir.path().join("graph.sqlite3"))
            .expect("sqlite repository must open");
        std::mem::forget(dir);
        AuditManager::new(
            Arc::new(repo),
            SolverRegistry::new(),
            Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        )
    }

    #[test]
    fn finding_v2_fingerprint_matches_python_fixed_id_probe() {
        let mut finding = Finding::new(
            ProjectId::new("proj_parity".to_string()),
            "manual finding".to_string(),
        );
        finding.rule_id = Some("parity.rule".to_string());
        assert_eq!(
            finding_fingerprint(&finding, &HashMap::new()),
            "v2:db82683718ee1a7f1332bc53"
        );
    }

    #[tokio::test]
    async fn manual_nodes_keep_reference_validation_in_manager() {
        let manager = manager();
        let project = manager
            .create_project("manual", AuditDomain::WebSast, None, None, None)
            .await
            .expect("project creation must succeed");
        let fact = manager
            .add_user_fact(
                project.id.as_str(),
                "endpoint",
                "/health",
                Map::new(),
                Vec::new(),
                1.0,
            )
            .expect("fact append must succeed");
        let evidence = manager
            .add_manual_evidence(
                project.id.as_str(),
                EvidenceKind::SourceSnippet,
                "handler",
                Map::new(),
                vec![fact.id.as_str().to_string()],
            )
            .expect("evidence append must succeed");
        let finding = manager
            .add_manual_finding(
                project.id.as_str(),
                "manual finding",
                None,
                Severity::Low,
                None,
                Some("manual.rule".to_string()),
                vec![evidence.id.as_str().to_string()],
                vec![fact.id.as_str().to_string()],
                None,
                None,
            )
            .await
            .expect("finding append must succeed");
        assert_eq!(finding.evidence_ids, vec![evidence.id.as_str().to_string()]);
        assert!(
            finding
                .fingerprint
                .as_deref()
                .is_some_and(|value| value.starts_with("v2:"))
        );
        let duplicate = manager
            .add_manual_finding(
                project.id.as_str(),
                "manual finding",
                None,
                Severity::Low,
                None,
                Some("manual.rule".to_string()),
                vec![evidence.id.as_str().to_string()],
                vec![fact.id.as_str().to_string()],
                None,
                None,
            )
            .await
            .expect("duplicate finding must remain auditable");
        assert_eq!(duplicate.status, FindingStatus::Duplicate);
        assert_eq!(duplicate.dedup_of.as_deref(), Some(finding.id.as_str()));
        let events = manager
            .repository()
            .list_events(project.id.as_str(), None, 100, None)
            .expect("finding events must be readable");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == AuditEventType::FindingAdded)
                .count(),
            2
        );
        let error = manager
            .add_manual_evidence(
                project.id.as_str(),
                EvidenceKind::ToolOutput,
                "bad reference",
                Map::new(),
                vec!["fact_missing".to_string()],
            )
            .expect_err("unknown fact must be rejected");
        assert!(matches!(error, EngineError::ReferenceValidationError(_)));
    }

    #[tokio::test]
    async fn knowledge_cards_normalize_and_validate_limits() {
        let manager = manager();
        let card = manager
            .add_knowledge_card(
                KnowledgeCardKind::ToolUsage,
                "  Curl  ",
                "  Follow redirects  ",
                vec![" Web ".to_string(), "web".to_string()],
                Some("manual".to_string()),
                80,
            )
            .expect("knowledge card must be stored");
        assert_eq!(card.title, "Curl");
        assert_eq!(card.tags, vec!["web"]);
        assert_eq!(
            manager
                .list_knowledge_cards()
                .expect("list must work")
                .len(),
            1
        );
        assert!(matches!(
            manager.add_knowledge_card(
                KnowledgeCardKind::ToolUsage,
                "",
                "content",
                Vec::new(),
                None,
                50,
            ),
            Err(EngineError::Value(_))
        ));
    }

    #[tokio::test]
    async fn artifact_metadata_requires_uri_and_preserves_project_scope() {
        let manager = manager();
        let project = manager
            .create_project("artifacts", AuditDomain::WebSast, None, None, None)
            .await
            .expect("project creation must succeed");
        let mut artifact = models::ArtifactRecord::new("report.sarif".to_string());
        artifact.project_id = Some(project.id.clone());
        artifact.size_bytes = Some(10);
        let stored = manager
            .add_artifact_record(&artifact)
            .expect("artifact metadata must be stored");
        assert_eq!(stored.project_id, Some(project.id));
        let mut invalid = models::ArtifactRecord::new(" ".to_string());
        invalid.size_bytes = Some(0);
        assert!(matches!(
            manager.add_artifact_record(&invalid),
            Err(EngineError::Value(_))
        ));
    }
}
