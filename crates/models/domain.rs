//! 形式化审计域词汇表 —— `server/core/models/common.py` 的 `AuditDomain` 移植。

use serde::{Deserialize, Serialize};

use crate::module::ModuleDomain;

/// Lynceus 引擎模型使用的形式化审计域（`AuditDomain`）。
///
/// 20 个域覆盖资产测绘到供应链审计的完整攻击面词汇表。变体顺序冻结自
/// Python 枚举定义序（`AuditDomain::ALL`）：`CoverageChecker` 按该顺序
/// 产出 `CoverageDomainEntry`，两侧顺序漂移会直接破坏对拍。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDomain {
    /// 资产测绘。
    AssetRecon,
    /// Web 侦察。
    WebRecon,
    /// 内容发现。
    ContentDiscovery,
    /// 指纹识别。
    FingerprintIntelligence,
    /// 暴露面情报。
    ExposureIntelligence,
    /// Web 静态分析。
    WebSast,
    /// Web 动态测试。
    WebDast,
    /// Web 交互式测试。
    WebIast,
    /// Web 漏洞验证。
    WebValidation,
    /// 可利用性验证。
    ExploitabilityValidation,
    /// 内网面。
    InternalSurface,
    /// 流量情报。
    TrafficIntelligence,
    /// 深度代码静态分析。
    CodeDeepSast,
    /// 二进制静态分析。
    BinaryStatic,
    /// 二进制动态分析。
    BinaryDynamic,
    /// 可利用性。
    Exploitability,
    /// 模糊测试。
    Fuzzing,
    /// 供应链。
    SupplyChain,
    /// 云原生。
    CloudNative,
    /// 复合（跨域）。
    Composite,
    /// 杂项（编码/解码、密码学谜题、隐写等无法归入其他类型的任务）。
    Misc,
}

impl AuditDomain {
    /// Python 枚举定义序全集（`for domain in AuditDomain` 的镜像）。
    pub const ALL: [AuditDomain; 21] = [
        AuditDomain::AssetRecon,
        AuditDomain::WebRecon,
        AuditDomain::ContentDiscovery,
        AuditDomain::FingerprintIntelligence,
        AuditDomain::ExposureIntelligence,
        AuditDomain::WebSast,
        AuditDomain::WebDast,
        AuditDomain::WebIast,
        AuditDomain::WebValidation,
        AuditDomain::ExploitabilityValidation,
        AuditDomain::InternalSurface,
        AuditDomain::TrafficIntelligence,
        AuditDomain::CodeDeepSast,
        AuditDomain::BinaryStatic,
        AuditDomain::BinaryDynamic,
        AuditDomain::Exploitability,
        AuditDomain::Fuzzing,
        AuditDomain::SupplyChain,
        AuditDomain::CloudNative,
        AuditDomain::Composite,
        AuditDomain::Misc,
    ];

    /// wire 值（Python `domain.value` 的镜像，用于文本拼接与排序键）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditDomain::AssetRecon => "asset_recon",
            AuditDomain::WebRecon => "web_recon",
            AuditDomain::ContentDiscovery => "content_discovery",
            AuditDomain::FingerprintIntelligence => "fingerprint_intelligence",
            AuditDomain::ExposureIntelligence => "exposure_intelligence",
            AuditDomain::WebSast => "web_sast",
            AuditDomain::WebDast => "web_dast",
            AuditDomain::WebIast => "web_iast",
            AuditDomain::WebValidation => "web_validation",
            AuditDomain::ExploitabilityValidation => "exploitability_validation",
            AuditDomain::InternalSurface => "internal_surface",
            AuditDomain::TrafficIntelligence => "traffic_intelligence",
            AuditDomain::CodeDeepSast => "code_deep_sast",
            AuditDomain::BinaryStatic => "binary_static",
            AuditDomain::BinaryDynamic => "binary_dynamic",
            AuditDomain::Exploitability => "exploitability",
            AuditDomain::Fuzzing => "fuzzing",
            AuditDomain::SupplyChain => "supply_chain",
            AuditDomain::CloudNative => "cloud_native",
            AuditDomain::Composite => "composite",
            AuditDomain::Misc => "misc",
        }
    }
}

/// Python `AuditDomain(ModuleDomain(domain).value)`：模块域按 wire 值
/// 折算为审计域。两个枚举当前值集相同，但不做静默兜底——某一侧新增
/// 值时此转换必须编译失败，让漂移在编译期暴露（Python 侧靠
/// `except ValueError: continue` 跳过，语义上等价于"无覆盖信号"）。
impl TryFrom<ModuleDomain> for AuditDomain {
    type Error = ModuleDomain;

    fn try_from(value: ModuleDomain) -> Result<Self, Self::Error> {
        Ok(match value {
            ModuleDomain::AssetRecon => AuditDomain::AssetRecon,
            ModuleDomain::WebRecon => AuditDomain::WebRecon,
            ModuleDomain::ContentDiscovery => AuditDomain::ContentDiscovery,
            ModuleDomain::FingerprintIntelligence => AuditDomain::FingerprintIntelligence,
            ModuleDomain::ExposureIntelligence => AuditDomain::ExposureIntelligence,
            ModuleDomain::WebSast => AuditDomain::WebSast,
            ModuleDomain::WebDast => AuditDomain::WebDast,
            ModuleDomain::WebIast => AuditDomain::WebIast,
            ModuleDomain::WebValidation => AuditDomain::WebValidation,
            ModuleDomain::ExploitabilityValidation => AuditDomain::ExploitabilityValidation,
            ModuleDomain::InternalSurface => AuditDomain::InternalSurface,
            ModuleDomain::TrafficIntelligence => AuditDomain::TrafficIntelligence,
            ModuleDomain::CodeDeepSast => AuditDomain::CodeDeepSast,
            ModuleDomain::BinaryStatic => AuditDomain::BinaryStatic,
            ModuleDomain::BinaryDynamic => AuditDomain::BinaryDynamic,
            ModuleDomain::Exploitability => AuditDomain::Exploitability,
            ModuleDomain::Fuzzing => AuditDomain::Fuzzing,
            ModuleDomain::SupplyChain => AuditDomain::SupplyChain,
            ModuleDomain::CloudNative => AuditDomain::CloudNative,
            ModuleDomain::Composite => AuditDomain::Composite,
        })
    }
}

/// 形式化审计域字符串无法解析。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown audit_domain {text:?}; available: {available}")]
pub struct AuditDomainParseError {
    /// 解析失败的原字符串。
    pub text: String,
    /// 合法值列表（逗号连接，与 Python 报错文本一致）。
    pub available: String,
}

/// Python `normalize_audit_domain`：接受大小写/连字符变体，拒绝未知值。
///
/// # Errors
/// 未知域值返回 [`AuditDomainParseError`]，报错文本与 Python 一致。
pub fn normalize_audit_domain(value: &str) -> Result<AuditDomain, AuditDomainParseError> {
    let normalized = value.trim().to_lowercase().replace('-', "_");
    AuditDomain::ALL
        .into_iter()
        .find(|domain| domain.as_str() == normalized)
        .ok_or_else(|| AuditDomainParseError {
            text: value.to_string(),
            available: AuditDomain::ALL
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// [`AuditDomain`] 的宽松解析：走 [`normalize_audit_domain`] 的规范化规则。
impl std::str::FromStr for AuditDomain {
    type Err = AuditDomainParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        normalize_audit_domain(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{assert_roundtrip, assert_wire_values};

    #[test]
    fn audit_domain_matches_python_wire_values_in_definition_order() {
        assert_wire_values(&[
            (AuditDomain::AssetRecon, "asset_recon"),
            (AuditDomain::WebRecon, "web_recon"),
            (AuditDomain::ContentDiscovery, "content_discovery"),
            (
                AuditDomain::FingerprintIntelligence,
                "fingerprint_intelligence",
            ),
            (AuditDomain::ExposureIntelligence, "exposure_intelligence"),
            (AuditDomain::WebSast, "web_sast"),
            (AuditDomain::WebDast, "web_dast"),
            (AuditDomain::WebIast, "web_iast"),
            (AuditDomain::WebValidation, "web_validation"),
            (
                AuditDomain::ExploitabilityValidation,
                "exploitability_validation",
            ),
            (AuditDomain::InternalSurface, "internal_surface"),
            (AuditDomain::TrafficIntelligence, "traffic_intelligence"),
            (AuditDomain::CodeDeepSast, "code_deep_sast"),
            (AuditDomain::BinaryStatic, "binary_static"),
            (AuditDomain::BinaryDynamic, "binary_dynamic"),
            (AuditDomain::Exploitability, "exploitability"),
            (AuditDomain::Fuzzing, "fuzzing"),
            (AuditDomain::SupplyChain, "supply_chain"),
            (AuditDomain::CloudNative, "cloud_native"),
            (AuditDomain::Composite, "composite"),
            (AuditDomain::Misc, "misc"),
        ]);
        assert_eq!(AuditDomain::ALL.len(), 21);
        for (index, domain) in AuditDomain::ALL.iter().enumerate() {
            assert_roundtrip(domain);
            // ALL 的顺序即枚举定义序，序号稳定（CoverageChecker 依赖）。
            assert_eq!(
                serde_json::to_string(domain).unwrap_or_default(),
                serde_json::to_string(&AuditDomain::ALL[index]).unwrap_or_default()
            );
        }
    }

    #[test]
    fn normalize_accepts_python_variants_and_rejects_unknown() {
        assert_eq!(normalize_audit_domain("WEB_SAST"), Ok(AuditDomain::WebSast));
        assert_eq!(
            normalize_audit_domain(" web-dast "),
            Ok(AuditDomain::WebDast)
        );
        let error = normalize_audit_domain("not_a_domain").unwrap_err();
        assert_eq!(error.text, "not_a_domain");
        assert!(error.available.contains("asset_recon"));
    }
}
