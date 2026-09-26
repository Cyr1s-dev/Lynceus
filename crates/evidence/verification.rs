//! 单一确定性确认策略 —— `server/core/evidence/verification.py` 的移植。
//!
//! Finding 被确认前的全部不可协商闸门在此汇合：Guardian（质量）+
//! Provenance Gate（出处）+ `flag_capture` 产品门（候选 flag 必须逐字节
//! 出现在密封工件里，工件指纹 = 磁盘字节 SHA-256）。

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use models::{
    Evidence, EvidenceId, Finding, FindingStatus, ToolInvocation, ToolInvocationId, ToolStatus,
};

use crate::Sha256Fingerprint;
use crate::guardian::Guardian;
use crate::provenance_gate::ProvenanceGate;

/// `flag_capture` 产品门回执（镜像 Python receipt dict 的 wire 形态）。
///
/// 落盘进 `finding.review["product_verification"]`，由
/// `TerminationEvaluator` 读回判定 `FLAG_CAPTURE` 目标是否已验证交付。
/// 键序与 Python dict 插入序一致，保证双跑 review JSON 逐字节一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductVerification {
    /// 回执 schema 版本（当前恒为 `product-verification.v1`）。
    pub schema_version: String,
    /// 是否验证通过。
    pub verified: bool,
    /// 产品门类型（当前恒为 `flag_capture`）。
    pub kind: String,
    /// 支撑验证的 Evidence。
    pub evidence_id: EvidenceId,
    /// 产出工件的 `ToolInvocation`。
    pub tool_invocation_id: ToolInvocationId,
    /// 工件磁盘字节的 SHA-256（小写十六进制）。
    pub artifact_sha256: String,
    /// 候选值字节的 SHA-256（小写十六进制）。
    pub candidate_sha256: String,
}

/// Guardian + Provenance + 产品门的合并裁决（镜像 Python
/// `FindingVerificationDecision` frozen dataclass）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingVerificationDecision {
    allowed: bool,
    reasons: Vec<String>,
    product_verification: Option<ProductVerification>,
}

impl FindingVerificationDecision {
    /// 是否放行确认。
    #[must_use]
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// 全部失败原因（Guardian 在前，去重保序）。
    #[must_use]
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }

    /// `flag_capture` 产品门回执（仅整体放行时存在）。
    #[must_use]
    pub fn product_verification(&self) -> Option<&ProductVerification> {
        self.product_verification.as_ref()
    }
}

/// Finding 确认前应用每一道不可协商闸门。
pub struct FindingVerificationService;

impl FindingVerificationService {
    /// 检查 *finding* 是否可确认。
    ///
    /// 以 `status = confirmed` 的候选副本跑 Guardian 与 Provenance，再按
    /// `rule_id` 路由产品门（当前仅 `web_exploit.flag_capture`）。
    #[must_use]
    pub fn check_confirmation(
        finding: &Finding,
        evidence_records: &[Evidence],
        tool_invocations: &[ToolInvocation],
    ) -> FindingVerificationDecision {
        let mut candidate = finding.clone();
        candidate.status = FindingStatus::Confirmed;
        let guardian = Guardian::check(&candidate, Some(evidence_records));
        let known_tool_invocation_ids: HashSet<String> = tool_invocations
            .iter()
            .map(|invocation| invocation.id.as_str().to_string())
            .collect();
        let provenance = ProvenanceGate::check_confirmation(
            &candidate,
            Some(evidence_records),
            Some(&known_tool_invocation_ids),
        );

        let mut reasons: Vec<String> = guardian.reasons().to_vec();
        if !provenance.allowed()
            && let Some(reason) = provenance.reason()
        {
            reasons.push(reason.to_string());
        }
        let mut product_verification = None;
        let mut product_allowed = true;
        if candidate.rule_id.as_deref() == Some("web_exploit.flag_capture") {
            let (allowed, product_reasons, verification) =
                Self::verify_flag_capture(&candidate, evidence_records, tool_invocations);
            product_allowed = allowed;
            product_verification = verification;
            reasons.extend(product_reasons);
        }
        let allowed = guardian.passed() && provenance.allowed() && product_allowed;
        let mut deduped: Vec<String> = Vec::with_capacity(reasons.len());
        let mut seen: HashSet<String> = HashSet::with_capacity(reasons.len());
        for reason in reasons {
            if seen.insert(reason.clone()) {
                deduped.push(reason);
            }
        }
        FindingVerificationDecision {
            allowed,
            reasons: deduped,
            product_verification: if allowed { product_verification } else { None },
        }
    }

    /// 只接受逐字节出现在密封工具工件里的 Flag。
    ///
    /// 指纹 = 磁盘真实字节的 SHA-256（最高红线）：先按 Evidence 记录的
    /// fingerprint 校验工件未被篡改，再要求候选值在原始字节中逐字出现。
    fn verify_flag_capture(
        finding: &Finding,
        evidence_records: &[Evidence],
        tool_invocations: &[ToolInvocation],
    ) -> (bool, Vec<String>, Option<ProductVerification>) {
        let evidence_by_id: HashMap<&str, &Evidence> = evidence_records
            .iter()
            .map(|evidence| (evidence.id.as_str(), evidence))
            .collect();
        let invocations: HashMap<&str, &ToolInvocation> = tool_invocations
            .iter()
            .map(|invocation| (invocation.id.as_str(), invocation))
            .collect();
        let mut failures: Vec<String> = Vec::new();
        for evidence_id in &finding.evidence_ids {
            let Some(evidence) = evidence_by_id.get(evidence_id.as_str()) else {
                continue;
            };
            let candidate = evidence
                .content
                .get("flag")
                .and_then(Value::as_str)
                .filter(|flag| !flag.is_empty());
            let Some(candidate) = candidate else {
                failures.push(format!(
                    "evidence {} has no structured Flag candidate",
                    evidence.id
                ));
                continue;
            };
            let invocation = evidence
                .produced_by_tool_invocation_id
                .as_ref()
                .and_then(|id| invocations.get(id.as_str()));
            let Some(invocation) = invocation else {
                failures.push(format!(
                    "evidence {} is not tied to the successful producing task",
                    evidence.id
                ));
                continue;
            };
            if invocation.status != ToolStatus::Ok
                || invocation.task_id != finding.produced_by_task_id
            {
                failures.push(format!(
                    "evidence {} is not tied to the successful producing task",
                    evidence.id
                ));
                continue;
            }
            let path = evidence.evidence_path.as_deref().unwrap_or_default();
            let fingerprint = evidence.fingerprint.as_deref().unwrap_or_default();
            let candidate_bytes = candidate.as_bytes();
            let artifact_sha256 =
                match verify_sealed_artifact(&evidence.id, path, fingerprint, candidate_bytes) {
                    Ok(digest) => digest,
                    Err(failure) => {
                        failures.push(failure);
                        continue;
                    }
                };
            return (
                true,
                Vec::new(),
                Some(ProductVerification {
                    schema_version: "product-verification.v1".to_string(),
                    verified: true,
                    kind: "flag_capture".to_string(),
                    evidence_id: evidence.id.clone(),
                    tool_invocation_id: invocation.id.clone(),
                    artifact_sha256,
                    candidate_sha256: Sha256Fingerprint::compute(candidate_bytes)
                        .as_hex()
                        .to_string(),
                }),
            );
        }
        if failures.is_empty() {
            failures.push("Flag capture has no verifiable response evidence".to_string());
        }
        (false, failures, None)
    }
}

/// 校验密封工件：记录指纹与磁盘字节一致，且候选值逐字出现在原始字节中。
///
/// 指纹 = 磁盘真实字节的 SHA-256（最高红线）：先按 Evidence 记录的
/// fingerprint 校验工件未被篡改，再要求候选值在原始字节中逐字出现。
///
/// # Errors
///
/// 返回带 Evidence 上下文的失败原因（措辞与 Python 侧逐字节一致，
/// 会进入 review JSON）：
/// - 工件路径或指纹为空；
/// - 工件不可读；
/// - 磁盘字节指纹与记录不符（换行污染/篡改/截断）；
/// - 候选值不在工件字节中逐字出现。
fn verify_sealed_artifact(
    evidence_id: &models::EvidenceId,
    path: &str,
    fingerprint: &str,
    candidate_bytes: &[u8],
) -> Result<String, String> {
    if path.is_empty() || fingerprint.is_empty() {
        return Err(format!(
            "evidence {evidence_id} has no sealed response artifact"
        ));
    }
    let raw = std::fs::read(Path::new(path))
        .map_err(|_| format!("evidence artifact is unreadable: {path}"))?;
    let actual_digest = Sha256Fingerprint::compute(&raw);
    let expected_digest = fingerprint.strip_prefix("sha256:").unwrap_or(fingerprint);
    if !actual_digest.as_hex().eq_ignore_ascii_case(expected_digest) {
        return Err(format!("evidence artifact digest mismatch: {evidence_id}"));
    }
    if !raw
        .windows(candidate_bytes.len())
        .any(|window| window == candidate_bytes)
    {
        return Err(format!(
            "Flag candidate is not present verbatim in evidence artifact {evidence_id}"
        ));
    }
    Ok(actual_digest.as_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{FindingId, ProjectId, RunId, Severity, TaskId};

    fn flag_fixture(
        candidate: &str,
        raw: &[u8],
        dir: &std::path::Path,
    ) -> (Finding, Evidence, ToolInvocation) {
        let artifact = dir.join("response.txt");
        std::fs::write(&artifact, raw).expect("工件必须可写入");
        let mut invocation = ToolInvocation::new("http_harvest".to_string(), "GET /".to_string());
        invocation.id = models::ToolInvocationId::new("ti-flag".to_string());
        invocation.project_id = Some(ProjectId::new("p1".to_string()));
        invocation.run_id = Some(RunId::new("r1".to_string()));
        invocation.task_id = Some(TaskId::new("task-flag".to_string()));
        invocation.status = ToolStatus::Ok;

        let mut evidence = Evidence::new(
            ProjectId::new("p1".to_string()),
            models::EvidenceKind::PocDescription,
            "Captured challenge result".to_string(),
        );
        evidence.id = EvidenceId::new("ev-flag".to_string());
        evidence.run_id = Some(RunId::new("r1".to_string()));
        evidence.produced_by_task_id = Some(TaskId::new("task-flag".to_string()));
        evidence.produced_by_tool_invocation_id = Some(invocation.id.clone());
        evidence
            .content
            .insert("flag".to_string(), Value::from(candidate));
        evidence.evidence_path = Some(artifact.to_string_lossy().into_owned());
        evidence.fingerprint = Some(Sha256Fingerprint::compute(raw).as_hex().to_string());

        let mut finding = Finding::new(
            ProjectId::new("p1".to_string()),
            "Verified challenge result".to_string(),
        );
        finding.id = FindingId::new("finding-flag".to_string());
        finding.run_id = Some(RunId::new("r1".to_string()));
        finding.produced_by_task_id = Some(TaskId::new("task-flag".to_string()));
        finding.description = Some("Recovered from the target response".to_string());
        finding.rule_id = Some("web_exploit.flag_capture".to_string());
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];
        finding.source_label = Some("HTTP response".to_string());
        finding.sink_label = Some("challenge result".to_string());
        finding.severity = Severity::Medium;
        (finding, evidence, invocation)
    }

    #[test]
    fn flag_product_gate_requires_verbatim_sealed_tool_output() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let candidate = "CTF2{verified-output}";
        let raw = format!("HTTP/1.1 200 OK\n\nresult={candidate}\n");
        let (finding, evidence, invocation) = flag_fixture(candidate, raw.as_bytes(), dir.path());

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(decision.allowed());
        let receipt = decision
            .product_verification()
            .expect("放行时必须携带产品门回执");
        assert!(receipt.verified);
        assert_eq!(receipt.schema_version, "product-verification.v1");
        assert_eq!(receipt.kind, "flag_capture");
        assert_eq!(receipt.evidence_id, evidence.id);
        assert_eq!(receipt.tool_invocation_id, invocation.id);
        assert_eq!(
            receipt.artifact_sha256,
            evidence.fingerprint.clone().unwrap_or_default()
        );
    }

    #[test]
    fn flag_product_gate_rejects_candidate_absent_from_artifact() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nno result here\n";
        let (finding, evidence, invocation) = flag_fixture("CTF2{invented}", raw, dir.path());

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(!decision.allowed());
        assert!(decision.product_verification().is_none());
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("not present verbatim")),
            "必须点名候选值不在工件内: {:?}",
            decision.reasons()
        );
    }

    #[test]
    fn flag_product_gate_rejects_digest_mismatch() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nresult=CTF2{verified-output}\n";
        let (mut finding, mut evidence, invocation) =
            flag_fixture("CTF2{verified-output}", raw, dir.path());
        // 指纹指向另一份字节：工件被篡改/换行污染的场景。
        evidence.fingerprint = Some(Sha256Fingerprint::compute(b"tampered").as_hex().to_string());
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(!decision.allowed());
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("digest mismatch")),
            "必须点名指纹失配: {:?}",
            decision.reasons()
        );
    }

    #[test]
    fn flag_product_gate_rejects_failed_invocation() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nresult=CTF2{verified-output}\n";
        let (finding, evidence, mut invocation) =
            flag_fixture("CTF2{verified-output}", raw, dir.path());
        invocation.status = ToolStatus::Error;

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(!decision.allowed());
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("not tied to the successful producing task")),
            "必须点名工具调用未成功: {:?}",
            decision.reasons()
        );
    }

    #[test]
    fn flag_product_gate_rejects_unstructured_candidate() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nno structured flag\n";
        let (mut finding, mut evidence, invocation) =
            flag_fixture("CTF2{verified-output}", raw, dir.path());
        evidence.content.remove("flag");
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(!decision.allowed());
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("no structured Flag candidate")),
            "必须点名候选值缺失: {:?}",
            decision.reasons()
        );
    }

    #[test]
    fn non_flag_rule_skips_product_gate() {
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nresult=CTF2{verified-output}\n";
        let (mut finding, evidence, invocation) =
            flag_fixture("CTF2{verified-output}", raw, dir.path());
        finding.rule_id = Some("web_exploit.ssrf".to_string());

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );

        assert!(decision.allowed());
        assert!(decision.product_verification().is_none());
    }

    #[test]
    fn product_verification_receipt_round_trips_through_review_json() {
        // 回执要写进 finding.review（Map<String, Value>），必须能无损往返。
        let dir = tempfile::tempdir().expect("系统临时目录应可创建");
        let raw = b"HTTP/1.1 200 OK\n\nresult=CTF2{verified-output}\n";
        let (finding, evidence, invocation) =
            flag_fixture("CTF2{verified-output}", raw, dir.path());

        let decision = FindingVerificationService::check_confirmation(
            &finding,
            std::slice::from_ref(&evidence),
            std::slice::from_ref(&invocation),
        );
        let receipt = decision
            .product_verification()
            .expect("放行时必须携带产品门回执");
        let value = serde_json::to_value(receipt).expect("回执必须可序列化");
        let back: ProductVerification = serde_json::from_value(value).expect("回执必须可反序列化");
        assert_eq!(back, *receipt);
    }
}
