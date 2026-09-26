//! Agent 预设 —— 三处硬编码提示词（worker 指令 / 任务顾问 / intake
//! analyze）收编为可编辑、可版本化的预设资产。
//!
//! wire 冻结纪律的延伸：提示词是效果核心资产。每个内置预设的 v1 模板
//! 必须与改造前的硬编码输出**逐字节一致**（parity 测试守护在
//! `engines/worker/dispatch.rs` 与 `api/intake.rs`）。
//!
//! 模板语法（v1，刻意保持最小）：
//! - `{{var}}`：变量插值；缺失或空字符串渲染为空串；
//! - `{{#if var}}...{{/if}}`：条件块，可嵌套；`var` 缺失 / 空串 /
//!   `false` / `null` 时整块跳过；
//! - 渲染结束后把 3 个以上连续换行折叠为段落空行（\n\n）——这让
//!   条件块之间可以安全地作者化空行分隔，被跳过的块不会留下双空行。

use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::common::Timestamp;

/// 内置预设 key：worker 任务指令。
pub const PRESET_WORKER_INSTRUCTION: &str = "worker_instruction";
/// 内置预设 key：任务顾问（`POST /missions/{id}/advise`）。
pub const PRESET_MISSION_ADVISOR: &str = "mission_advisor";
/// 内置预设 key：intake analyze（自然语言 → 计划的结构化生成）。
pub const PRESET_INTAKE_ANALYZE: &str = "intake_analyze";
/// 内置预设 key：策略板维护（StrategyBoardMaintainer sidecar）。
pub const PRESET_STRATEGY_BOARD_MAINTAINER: &str = "strategy_board_maintainer";
/// 内置预设 key：元认知发散（Mission 收尾链的发散阶段）。
pub const PRESET_METACOGNITION_DIVERGENCE: &str = "metacognition_divergence";

/// 全部内置预设 key（模型分工目录）。
pub const BUILTIN_KEYS: [&str; 5] = [
    PRESET_WORKER_INSTRUCTION,
    PRESET_MISSION_ADVISOR,
    PRESET_INTAKE_ANALYZE,
    PRESET_STRATEGY_BOARD_MAINTAINER,
    PRESET_METACOGNITION_DIVERGENCE,
];

/// Agent 预设：key 唯一（同时是存储 id）；`builtin` 预设可编辑不可删除。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPreset {
    /// 稳定 key（如 `worker_instruction`）。
    pub key: String,
    /// 展示名。
    pub name: String,
    /// 描述。
    #[serde(default)]
    pub description: Option<String>,
    /// 是否内置预设（内置不可删除，可编辑）。
    pub builtin: bool,
    /// 是否启用；停用的预设不参与运行时解析。
    pub enabled: bool,
    /// 模型别名覆盖（可选；缺省用 runtime 默认绑定）。
    #[serde(default)]
    pub model_alias: Option<String>,
    /// 步数预算覆盖（可选）。
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// 当前生效的指令模板。
    pub instruction_template: String,
    /// 模板声明的变量名（按出现序去重；写入时提取）。
    #[serde(default)]
    pub variables: Vec<String>,
    /// 收尾提示词模板（可选）。
    #[serde(default)]
    pub wrapup_template: Option<String>,
    /// Skill 可见性白名单（WP6）：非空 = 该预设的 worker 只能 load 列出的
    /// skill；空 = 全部可见。
    #[serde(default)]
    pub skills: Vec<String>,
    /// 工具授权白名单（WP5）：非空 = 该预设的 worker 的 MCP 会话只允许
    /// 列出的工具目录条目（与 grant allowlist 取交集，fail-closed）；
    /// 空 = 不限制。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
}

impl AgentPreset {
    /// 以给定模板构造 v1 内置预设（变量自动提取）。
    #[must_use]
    pub fn new_v1(
        key: impl Into<String>,
        name: impl Into<String>,
        description: Option<String>,
        instruction_template: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        let instruction_template = instruction_template.into();
        let variables = extract_variables(&instruction_template);
        Self {
            key: key.into(),
            name: name.into(),
            description,
            builtin: true,
            enabled: true,
            model_alias: None,
            max_turns: None,
            instruction_template: instruction_template.clone(),
            wrapup_template: None,
            skills: Vec::new(),
            tools: Vec::new(),
            variables,
            created_at,
            updated_at: created_at,
        }
    }
}

/// 从模板提取 `{{var}}` 变量名（跳过 `{{#if}}`/`{{/if}}`，按出现序去重）。
#[must_use]
pub fn extract_variables(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if let Some(end_rel) = template[i + 2..].find("}}") {
                let inner = &template[i + 2..i + 2 + end_rel];
                let name = inner.trim();
                let is_block = name.starts_with('#') || name.starts_with('/');
                if !is_block && !name.is_empty() && !names.iter().any(|v| v == name) {
                    names.push(name.to_string());
                }
                i += 2 + end_rel + 2;
                continue;
            }
        }
        i += 1;
    }
    names
}

/// 条件变量的真值：非空字符串且不等于 "false"（bool/null/缺失为假）。
fn truthy(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::String(s)) => !s.is_empty() && s != "false",
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Null) | None => false,
        Some(serde_json::Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0),
        Some(_) => true,
    }
}

/// 变量插值：缺失渲染为空串；`{{#if}}`/`{{/if}}` 块语法保持原样。
fn substitute(text: &str, vars: &Map<String, serde_json::Value>) -> String {
    // 按 char 迭代（bytes[i] as char 会把多字节 UTF-8 拆成乱码）。
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '{' && text[i..].starts_with("{{") {
            if let Some(end_rel) = text[i + 2..].find("}}") {
                let tag_len = 2 + end_rel + 2;
                let inner = &text[i + 2..i + 2 + end_rel];
                let name = inner.trim();
                if name.starts_with('#') || name.starts_with('/') {
                    // 块语法不属于变量插值；交给 render_scopes / 原文回退处理。
                    out.push_str(&text[i..i + tag_len]);
                } else {
                    let substituted = match vars.get(name) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Number(n)) => n.to_string(),
                        Some(serde_json::Value::Bool(b)) => b.to_string(),
                        _ => String::new(),
                    };
                    out.push_str(&substituted);
                }
                // 消费掉标签剩余字符（首个 '{' 已消费）。
                for _ in 1..tag_len {
                    chars.next();
                }
                continue;
            }
        }
        out.push(ch);
    }
    out
}

/// 渲染模板：条件块求值 + 变量插值 + 空行折叠。语法非法（未闭合的
/// if 块）时按原文变量插值收尾——预设编辑在 API 侧校验，运行时绝不
/// 因模板语法中断任务派发。
#[must_use]
pub fn render_template(template: &str, vars: &Map<String, serde_json::Value>) -> String {
    let rendered = match render_scopes(template, vars) {
        Some(rendered) => rendered,
        None => substitute(template, vars),
    };
    collapse_blank_lines(&rendered)
}

/// 递归渲染（None = if 块未闭合，调用方回退）。
fn render_scopes(text: &str, vars: &Map<String, serde_json::Value>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(boundary) = find_block_boundary(rest) else {
            // 纯文本（无块边界）：变量插值后收尾。
            out.push_str(&substitute(rest, vars));
            return Some(out);
        };
        let (before, after_boundary) = rest.split_at(boundary);
        out.push_str(&substitute(before, vars));
        if after_boundary.starts_with("{{/if}}") {
            // 孤立关闭符：语法非法，整体回退。
            return None;
        }
        let after_open = &after_boundary["{{#if".len()..];
        let close_rel = after_open.find("}}")?;
        let name = after_open[..close_rel].trim();
        let after_open = &after_open[close_rel + 2..];
        let (inner, after_close) = find_block_close(after_open)?;
        if truthy(vars.get(name)) {
            out.push_str(&render_scopes(&inner, vars)?);
        }
        rest = after_close;
    }
}

/// 下一个块边界（`{{#if` 或 `{{/if}}`）。
fn find_block_boundary(text: &str) -> Option<usize> {
    let if_pos = text.find("{{#if");
    let close_pos = text.find("{{/if}}");
    match (if_pos, close_pos) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 找与当前 if 匹配的 `{{/if}}`（支持嵌套），返回（块体，闭合后文本）。
fn find_block_close(text: &str) -> Option<(String, &str)> {
    let mut depth = 1usize;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            if text[i..].starts_with("{{#if") {
                depth += 1;
                i += 5;
                continue;
            }
            if text[i..].starts_with("{{/if}}") {
                depth -= 1;
                if depth == 0 {
                    return Some((text[..i].to_string(), &text[i + 7..]));
                }
                i += 7;
                continue;
            }
            if let Some(end_rel) = text[i + 2..].find("}}") {
                i += end_rel + 2;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// 折叠 3 个以上连续换行为段落空行（`\n\n`）。
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    let mut flush = |out: &mut String, run: usize| {
        if run >= 3 {
            out.push_str("\n\n");
        } else {
            for _ in 0..run {
                out.push('\n');
            }
        }
    };
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
        } else {
            flush(&mut out, newline_run);
            newline_run = 0;
            out.push(ch);
        }
    }
    flush(&mut out, newline_run);
    out
}

/// 预设来源抽象：运行时按 key 取启用预设（仓储适配器实现）。
pub trait AgentPresetSource: Send + Sync {
    /// 取启用中的预设；停用 / 缺失返回 `None`（调用方回落下一优先级）。
    fn enabled_preset(&self, key: &str) -> Option<AgentPreset>;
}

/// 三个内置预设的 v1 模板（wire 冻结：与改造前硬编码逐字节一致，
/// parity 测试守护在 dispatch.rs 与 intake.rs）。
pub mod builtin_templates {
    /// 内置默认提示词以 **文件存储**（`resources/prompts/*.md`），经
    /// `include_str!` 进入二进制——源是文件而非代码字面量。文件内容与
    /// 改造前硬编码逐字节一致（parity 测试守护）。
    pub const WORKER_INSTRUCTION_V1: &str =
        include_str!("../../resources/prompts/worker_instruction.md");

    pub const MISSION_ADVISOR_V1: &str =
        include_str!("../../resources/prompts/mission_advisor.md");

    pub const INTAKE_ANALYZE_V1: &str =
        include_str!("../../resources/prompts/intake_analyze.md");

    pub const STRATEGY_BOARD_MAINTAINER_V1: &str =
        include_str!("../../resources/prompts/strategy_board_maintainer.md");

    pub const METACOGNITION_DIVERGENCE_V1: &str =
        include_str!("../../resources/prompts/metacognition_divergence.md");

    /// P1（出洞纪律对齐）之前的 worker 指令内置模板。
    ///
    /// 内置模板演进同步用：播种是 first-insert-only，新模板到不了已播种
    /// 的 DB 行。`builtin_legacy_templates` 以本常量为"用户未编辑过"的
    /// 判定基准——DB 行与之逐字节相同才允许覆盖。
    pub const LEGACY_WORKER_INSTRUCTION_V1: &str = r#"你正在以外部 worker 运行时（{{runtime_display_name}}）的身份，为 Lynceus 平台执行 '{{solver_name}}' 审计域。

{{#if has_intent}}
分支意图：{{intent_title}}
{{#if has_intent_description}}
意图详情：{{intent_description}}
{{/if}}
{{/if}}

{{#if has_targets}}
项目目标：{{targets}}
{{/if}}

{{#if has_mission_goal}}
任务目标：{{mission_goal}}
{{/if}}

本任务的步骤预算：{{budget_steps}}（有界执行，预算耗尽即停止）。

输出契约：以有界纯文本总结收尾，说明你检查了什么、发现了什么。不得声称已确认的发现——所有输出都会被 Lynceus 证据流水线视为受控观察。
"#;
}

/// 内置模板的历代版本（preset key → 上一个内置模板原文）。
///
/// 播种幂等（first-insert-only）与"用户编辑不被重启覆盖"是同一枚硬币
/// 的两面：内置模板升级时，已播种的 DB 行不会跟着升级。启动同步以
/// 这里的历史版本为判定基准——DB 行与历史版本逐字节相同即用户从未
/// 编辑，用新内置模板覆盖；差一个字节就不动（那是用户的编辑）。
#[must_use]
pub fn builtin_legacy_templates() -> &'static [(&'static str, &'static str)] {
    &[(
        PRESET_WORKER_INSTRUCTION,
        builtin_templates::LEGACY_WORKER_INSTRUCTION_V1,
    )]
}

/// 全部内置预设（启用、builtin、单版本 v1）。种子幂等：key 已存在
/// 时不覆盖——用户对内置预设的编辑不会在重启后丢失。
#[must_use]
pub fn builtin_seed_presets(created_at: Timestamp) -> Vec<AgentPreset> {
    vec![
        AgentPreset::new_v1(
            PRESET_WORKER_INSTRUCTION,
            "Worker 任务指令",
            Some("分支任务派发给外部 worker 的任务指令模板（目标 / intent / 预算 / 输出契约）".to_string()),
            builtin_templates::WORKER_INSTRUCTION_V1,
            created_at,
        ),
        AgentPreset::new_v1(
            PRESET_MISSION_ADVISOR,
            "任务顾问",
            Some("任务页顾问问答的只读侧分析 worker 指令（读白板 → 中文回答 → 结论写回）".to_string()),
            builtin_templates::MISSION_ADVISOR_V1,
            created_at,
        ),
        AgentPreset::new_v1(
            PRESET_INTAKE_ANALYZE,
            "Intake 分析",
            Some("自然语言输入 → 项目草稿与流水线计划的结构化生成系统提示词".to_string()),
            builtin_templates::INTAKE_ANALYZE_V1,
            created_at,
        ),
        AgentPreset::new_v1(
            PRESET_STRATEGY_BOARD_MAINTAINER,
            "策略板维护",
            Some("StrategyBoardMaintainer sidecar：压缩、连续性与求解效率优先的看板维护".to_string()),
            builtin_templates::STRATEGY_BOARD_MAINTAINER_V1,
            created_at,
        ),
        AgentPreset::new_v1(
            PRESET_METACOGNITION_DIVERGENCE,
            "元认知发散",
            Some("Mission 收尾链发散阶段：五框架产出未考察方向的证伪式猜想".to_string()),
            builtin_templates::METACOGNITION_DIVERGENCE_V1,
            created_at,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::utcnow;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    #[test]
    fn debug_trace2() {
        let v = vars(&[("x", "1"), ("y", "1"), ("z", "1")]);
        let tpl = "A
{{#if x}}X
{{#if y}}Y{{/if}}{{/if}}
{{#if z}}Z{{/if}}
B";
        let boundary = find_block_boundary(tpl);
        println!("boundary: {:?}", boundary);
        let after_open = &tpl[boundary.unwrap()..]["{{#if".len()..];
        let close_rel = after_open.find("}}").unwrap();
        let name = after_open[..close_rel].trim();
        let after_open2 = &after_open[close_rel + 2..];
        println!("name: {:?}", name);
        let fc = find_block_close(after_open2);
        println!("find_block_close: {:?}", fc.as_ref().map(|(i, a)| (i, a.len())));
        match &fc {
            Some((inner, _)) => println!("inner: {:?}", inner),
            None => {}
        }
        println!("scopes: {:?}", render_scopes(tpl, &v));
    }

    #[test]
    fn substitute_replaces_and_extracts_variables() {
        let names = extract_variables("Hello {{name}} and {{ name }}; {{#if flag}}x{{/if}}");
        assert_eq!(names, ["name"]);
        let out = substitute("Hi {{name}} / {{missing}} / {{name}}", &vars(&[("name", "世界")]));
        assert_eq!(out, "Hi 世界 /  / 世界");
    }

    #[test]
    fn if_blocks_skip_and_render_with_nesting() {
        let tpl = "A\n{{#if x}}X\n{{#if y}}Y{{/if}}{{/if}}\n{{#if z}}Z{{/if}}\nB";
        assert_eq!(
            render_template(tpl, &vars(&[("x", "1"), ("y", "1"), ("z", "1")])),
            "A\nX\nY\nZ\nB"
        );
        assert_eq!(
            render_template(tpl, &vars(&[("x", "1")])),
            "A\nX\n\nB",
            "跳过的 z 块连同作者空行折叠为一个段落空行"
        );
        assert_eq!(
            render_template(tpl, &vars(&[])),
            "A\n\nB",
            "全部跳过后不留多段空行"
        );
    }

    #[test]
    fn unclosed_block_falls_back_to_plain_substitution() {
        let tpl = "A {{name}} {{#if x}}open";
        assert_eq!(render_template(tpl, &vars(&[("name", "n")])), "A n {{#if x}}open");
    }

    #[test]
    fn builtin_seeds_are_unique_enabled_and_single_version() {
        let presets = builtin_seed_presets(utcnow());
        let mut keys: Vec<&str> = presets.iter().map(|p| p.key.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), presets.len(), "key 必须唯一");
        for preset in &presets {
            assert!(preset.builtin);
            assert!(preset.enabled);
            assert!(!preset.instruction_template.is_empty());
            assert_eq!(
                preset.variables,
                extract_variables(&preset.instruction_template),
                "variables 必须与模板提取一致（intake analyze 为纯静态提示词，可无变量）"
            );
        }
    }
}
