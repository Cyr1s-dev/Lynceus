//! 分工系统提示词解析（WP4）——任务画像 / 策略板维护 / 元认知发散这三处
//! 固定调用点没有 per-run 预设选择逻辑，经进程级覆盖表接 Agent 预设：
//!
//! - 组合根（api 启动）与预设变更后，把 `agent_presets` 中**启用**的
//!   预设模板刷进覆盖表；
//! - [`system_prompt`] 按 key 解析：覆盖表命中 → DB 模板；否则回落
//!   内置默认（`resources/prompts/*.md` 文件，`include_str!` 逐字节）。
//!
//! 全局态的取舍：提示词覆盖是应用级配置而非 per-request 状态，进程单例
//! 避免把仓储句柄穿透 agents 各条调用链；测试可用 [`clear_all`] 复位。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::RwLock;

static OVERRIDES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, String>> {
    OVERRIDES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 设置 / 更新一个预设覆盖（key → 启用中的模板）。
pub fn set_override(key: impl Into<String>, template: impl Into<String>) {
    if let Ok(mut guard) = store().write() {
        guard.insert(key.into(), template.into());
    }
}

/// 移除一个覆盖（预设停用 / 删除时），回落内置默认。
pub fn clear_override(key: &str) {
    if let Ok(mut guard) = store().write() {
        guard.remove(key);
    }
}

/// 全量重建覆盖表（启动播种后调用）。
pub fn set_overrides(map: HashMap<String, String>) {
    if let Ok(mut guard) = store().write() {
        *guard = map;
    }
}

/// 测试复位。
#[cfg(test)]
pub fn clear_all() {
    if let Ok(mut guard) = store().write() {
        guard.clear();
    }
}

/// 覆盖表快照（调试 / 健康端点用）。
#[must_use]
pub fn overrides() -> HashMap<String, String> {
    store().read().map(|guard| guard.clone()).unwrap_or_default()
}

/// 分工系统提示词解析：覆盖优先，回落内置默认（`builtin` 为
/// `resources/prompts` 文件的 `include_str!` 常量）。
#[must_use]
pub fn system_prompt(key: &str, builtin: &str) -> String {
    store()
        .read()
        .ok()
        .and_then(|guard| guard.get(key).cloned())
        .unwrap_or_else(|| builtin.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_takes_priority_and_clear_falls_back() {
        clear_all();
        assert_eq!(system_prompt("k", "BUILTIN"), "BUILTIN");
        set_override("k", "CUSTOM {{x}}");
        assert_eq!(system_prompt("k", "BUILTIN"), "CUSTOM {{x}}");
        clear_override("k");
        assert_eq!(system_prompt("k", "BUILTIN"), "BUILTIN");
        clear_all();
    }
}
