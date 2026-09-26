//! Tool Retrieval 最小评测（spec #40）：对完整 Catalog（全部条目强制
//! 可用）+ 原生/远程描述符执行检索，统计 Recall@5 / MRR / No-tool
//! accuracy / Explicit-tool hit rate。
//!
//! 评测是 hermetic 的：不触碰 PATH 检测、不调用任何模型、不执行工具。
//! fixtures 只声明语义契约（relevant 集合 / 显式期望 / no-tool 期望），
//! 不断言逐字分数。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use agents::tool_retrieval::ToolRetriever;
use engines::tool_catalog::ToolAvailability;
use engines::tool_catalog::ToolCatalogEntry;
use engines::tool_catalog::ToolDetection;
use engines::tool_catalog::load_catalog;
use engines::tool_retrieval::CatalogToolRetriever;
use engines::tool_retrieval::ToolRetrievalIndex;
use engines::tool_retrieval::all_native_descriptors;
use engines::tool_retrieval::ida_mcp_descriptor;
use models::tool_retrieval::ToolRetrievalQuery;
use serde::Deserialize;

const FIXTURE: &str = include_str!("fixtures/tool_retrieval_eval.json");

const TOP_K: usize = 5;
/// Recall@5 下限。
const MIN_RECALL_AT_5: f64 = 0.7;
/// MRR 下限。
const MIN_MRR: f64 = 0.6;

#[derive(Debug, Deserialize)]
struct EvalFixture {
    cases: Vec<EvalCase>,
}

#[derive(Debug, Deserialize)]
struct EvalCase {
    name: String,
    text: String,
    #[serde(default)]
    artifact_kinds: Vec<String>,
    #[serde(default)]
    explicit_tools: Vec<String>,
    #[serde(default)]
    relevant: Vec<String>,
    #[serde(default)]
    expect_no_tools: bool,
    #[serde(default)]
    explicit_expect: Option<String>,
}

/// 强制全部 catalog 条目可用（评测检索质量，不评测本机 PATH 环境）。
fn eval_retriever() -> CatalogToolRetriever {
    let tools = load_catalog().expect("embedded catalog must parse");
    let entries: Vec<ToolCatalogEntry> = tools
        .into_iter()
        .map(|tool| {
            let detection = ToolDetection {
                available: true,
                availability: ToolAvailability::Path,
                executable_path: Some(format!("/eval/bin/{}", tool.id)),
                source: None,
                version: None,
                sha256: None,
                source_url: None,
                integrity_status: "unverified".to_string(),
                integrity_message: None,
            };
            ToolCatalogEntry::from_tool(tool, detection, None)
        })
        .collect();
    let mut extra = all_native_descriptors();
    extra.push(ida_mcp_descriptor());
    let index = ToolRetrievalIndex::from_catalog(&entries).with_extra_descriptors(extra);
    CatalogToolRetriever::from_index(index)
}

/// 评测指标累加器（recall / MRR / no-tool / explicit 四族计数）。
#[derive(Default)]
struct EvalStats {
    recall_cases: usize,
    recall_sum: f64,
    mrr_sum: f64,
    no_tool_cases: usize,
    no_tool_hits: usize,
    explicit_cases: usize,
    explicit_hits: usize,
}

impl EvalStats {
    fn record(&mut self, case: &EvalCase, retriever: &CatalogToolRetriever) {
        let query = ToolRetrievalQuery {
            text: case.text.clone(),
            capability_queries: Vec::new(),
            artifact_kinds: case.artifact_kinds.clone(),
            explicit_tools: case.explicit_tools.clone(),
            risk_limit: None,
            limit: 10,
        };
        let result = retriever.retrieve(&query);
        let top_ids: Vec<String> = result
            .candidates
            .iter()
            .take(TOP_K)
            .map(|candidate| candidate.tool_id().to_lowercase())
            .collect();
        let relevant: Vec<String> = case.relevant.iter().map(|id| id.to_lowercase()).collect();

        if case.expect_no_tools {
            self.no_tool_cases += 1;
            if result.candidates.is_empty() {
                self.no_tool_hits += 1;
            } else {
                eprintln!(
                    "no-tool case `{}` unexpectedly returned: {top_ids:?}",
                    case.name
                );
            }
        }
        if !relevant.is_empty() {
            self.recall_cases += 1;
            let hits = relevant.iter().filter(|id| top_ids.contains(id)).count();
            self.recall_sum += hits as f64 / relevant.len() as f64;
            let rank = result
                .candidates
                .iter()
                .position(|candidate| relevant.contains(&candidate.tool_id().to_lowercase()));
            if let Some(rank) = rank {
                self.mrr_sum += 1.0 / (rank as f64 + 1.0);
            } else {
                eprintln!("case `{}` missed all relevant ids: {top_ids:?}", case.name);
            }
        }
        if let Some(expect) = &case.explicit_expect {
            self.explicit_cases += 1;
            let top = result
                .candidates
                .first()
                .map(|candidate| candidate.tool_id().to_lowercase());
            if top.as_deref() == Some(expect.to_lowercase().as_str()) {
                self.explicit_hits += 1;
            } else {
                eprintln!(
                    "explicit case `{}` expected `{}` at rank 1, got {top:?}",
                    case.name, expect
                );
            }
        }
    }

    fn recall_at_5(&self) -> f64 {
        if self.recall_cases == 0 {
            1.0
        } else {
            self.recall_sum / self.recall_cases as f64
        }
    }

    fn mrr(&self) -> f64 {
        if self.recall_cases == 0 {
            1.0
        } else {
            self.mrr_sum / self.recall_cases as f64
        }
    }

    fn no_tool_accuracy(&self) -> f64 {
        if self.no_tool_cases == 0 {
            1.0
        } else {
            self.no_tool_hits as f64 / self.no_tool_cases as f64
        }
    }

    fn explicit_hit_rate(&self) -> f64 {
        if self.explicit_cases == 0 {
            1.0
        } else {
            self.explicit_hits as f64 / self.explicit_cases as f64
        }
    }
}

#[test]
fn tool_retrieval_eval_meets_quality_thresholds() {
    let fixture: EvalFixture = serde_json::from_str(FIXTURE).expect("fixture must parse");
    assert!(!fixture.cases.is_empty(), "fixture must declare cases");
    let retriever = eval_retriever();

    let mut stats = EvalStats::default();
    for case in &fixture.cases {
        stats.record(case, &retriever);
    }

    let recall_at_5 = stats.recall_at_5();
    let mrr = stats.mrr();
    let no_tool_accuracy = stats.no_tool_accuracy();
    let explicit_hit_rate = stats.explicit_hit_rate();

    eprintln!(
        "tool retrieval eval: cases={} recall@{TOP_K}={recall_at_5:.3} mrr={mrr:.3} \
         no_tool_accuracy={no_tool_accuracy:.3} explicit_hit_rate={explicit_hit_rate:.3}",
        fixture.cases.len()
    );
    assert!(
        recall_at_5 >= MIN_RECALL_AT_5,
        "Recall@{TOP_K} {recall_at_5:.3} below {MIN_RECALL_AT_5}"
    );
    assert!(mrr >= MIN_MRR, "MRR {mrr:.3} below {MIN_MRR}");
    assert!(
        (no_tool_accuracy - 1.0).abs() < f64::EPSILON,
        "no-tool accuracy must be exact"
    );
    assert!(
        (explicit_hit_rate - 1.0).abs() < f64::EPSILON,
        "explicit-tool hit rate must be exact"
    );
}
