//! 命令策略（command policy）：worker 执行的禁区清单。
//!
//! # 为什么是两层，而不是一层
//!
//! 参考实现（norma SDK 自带 agent 循环）能把拦截点放在"模型→工具"之间，
//! 那是真正的 pre-execution gate。Lynceus 的 worker 是**黑盒 CLI 子进程**
//! （codex/claude/pi/dsh 各自在自家进程里跑 agent 循环），Lynceus 看到任
//! 何消息时命令早已执行完——进程内拦截在这个架构下不存在。
//!
//! 因此本策略分两层落地，各自覆盖各自能真拦的面：
//!
//! 1. **原生层（真拦截）**——译成每个 CLI 自己的 deny 面：
//!    - Claude Code `--disallowedTools "Bash(<prefix> *)"`（前缀匹配，
//!      实测该 CLI 支持）；
//!    - Lynceus 自己的 [`crate::tool_gateway`] spawn 前检查（MCP 工具面，
//!      完全自有）。
//!    只有"命令起始即可判定"的条目进这一层——前缀匹配零误杀。
//! 2. **提示词层（协作）**——全部条目（含 SQL/管道类无法前缀匹配的）以
//!    禁则块追加到 worker 指令尾部。**代码所有、不可被预设编辑掉**（与
//!    参考实现的 code-owned tail 同构），覆盖 codex/pi/dsh 这些没有
//!    per-command 原生层的 CLI。
//!
//! # 残余风险（如实声明，不许包装成"已防护"）
//!
//! 黑盒 CLI 的模型决定调用什么命令时，Lynceus 不在链路上；原生层只覆盖
//! Claude Code 与 Lynceus 自有工具面。codex/pi/dsh 的命令完全依赖模型
//! 配合提示词禁则。本策略是"原生层尽量真拦 + 提示词层兜底"，不是沙箱。

use serde_json::Map;
use serde_json::Value;

/// solver config 键：命令策略覆盖（JSON：`{"enabled": bool,
/// "denied_prefixes": [string]}`）。缺省 = [`CommandPolicy::seed`]。
pub const CONFIG_COMMAND_POLICY: &str = "worker_command_policy";

/// 默认禁则前缀（命令起始即判定；对齐参考实现 intercept 规则集的
/// 可前缀化子集 + Windows 等价项）。
///
/// 收录标准只有一条：**出现在命令开头就能判定、正常审计流程永远跑不到**。
/// 正常 worker 的活儿（curl/nmap/python 脚本/写文件）不在其列。
pub const DEFAULT_DENIED_PREFIXES: &[&str] = &[
    // 递归强制删除（根目录 / 系统盘级）
    "rm -rf /",
    "rm -fr /",
    "rm -rf C:\\",
    "rm -fr C:\\",
    "rm -rf C:/",
    "rm -fr C:/",
    "rd /s C:\\",
    "rd /s C:/",
    // 磁盘级破坏
    "mkfs",
    "dd of=/dev/",
    "shred",
    "wipefs",
    "format",
    "diskpart",
    // 系统可用性
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init 0",
    "init 6",
    "Stop-Computer",
    "Restart-Computer",
    "kill -9 -1",
    "killall -9",
    // 主机防火墙清空（会把worker自己的出网也打掉）
    "iptables -F",
    "iptables --flush",
    "nft flush",
];

/// 提示词层额外禁则（无法用命令前缀判定的形态：SQL 语句、HTTP 方法、
/// 批量清空接口——它们藏在参数里，只能靠模型配合）。
const PROMPT_ONLY_RULES: &[&str] = &[
    "SQL：DROP DATABASE / TABLE / SCHEMA / INDEX / VIEW、TRUNCATE TABLE",
    "MongoDB：dropDatabase() / dropCollection()；Redis：FLUSHALL / FLUSHDB",
    "curl / wget / requests / httpx 发 DELETE 请求（-X DELETE / --method DELETE / .delete(）",
    "axios.delete( 或 method: 'DELETE'",
    "请求 /clear /wipe /flush /purge /truncate /drop /destroy /factory-reset /reset-all 这类批量清空接口",
];

/// 命令策略：禁则前缀表 + 渲染（原生 deny 面 / 提示词块）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandPolicy {
    /// 禁则前缀（命令起始匹配；大小写不敏感）。
    pub denied_prefixes: Vec<String>,
}

impl CommandPolicy {
    /// 默认种子策略（开启 + 全量默认前缀）。
    #[must_use]
    pub fn seed() -> Self {
        Self {
            denied_prefixes: DEFAULT_DENIED_PREFIXES
                .iter()
                .map(|prefix| (*prefix).to_string())
                .collect(),
        }
    }

    /// 提示词层禁则（无法用命令前缀判定、只能靠模型配合的形态：SQL /
    /// DELETE / 批量清空接口），供工具审计页的「拦截规则」面板展示。
    #[must_use]
    pub fn prompt_only_rules() -> &'static [&'static str] {
        PROMPT_ONLY_RULES
    }

    /// 从 solver config 解析；缺省 = 种子策略。
    ///
    /// `{"enabled": false}` = 显式关闭（用户要为某个 mission 放开全部
    /// 禁则时的出口）；`denied_prefixes` 缺省 = 默认表。
    #[must_use]
    pub fn from_config(config: &Map<String, Value>) -> Self {
        let Some(policy) = config.get(CONFIG_COMMAND_POLICY) else {
            return Self::seed();
        };
        let enabled = policy
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            return Self::default();
        }
        let denied_prefixes = policy
            .get("denied_prefixes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|prefix| !prefix.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| Self::seed().denied_prefixes);
        Self { denied_prefixes }
    }

    /// 是否为空（无禁则 = 不渲染任何面）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.denied_prefixes.is_empty()
    }

    /// 命中判定：返回被禁的前缀（未命中 None）。
    ///
    /// 归一化：小写 + 空白折叠。前缀按词边界匹配——`format` 不误伤
    /// `formatted_output.txt` 这类以禁则词开头的正常参数。
    #[must_use]
    pub fn violation(&self, command_line: &str) -> Option<&str> {
        let normalized = normalize_command(command_line);
        self.denied_prefixes
            .iter()
            .find(|prefix| {
                let normalized_prefix = normalize_command(prefix);
                normalized == normalized_prefix
                    || (normalized.starts_with(&normalized_prefix)
                        && normalized[normalized_prefix.len()..]
                            .starts_with(|c: char| c.is_whitespace()))
            })
            .map(String::as_str)
    }

    /// Claude Code `--disallowedTools` 参数值（逗号分隔的 Bash 前缀模式）。
    #[must_use]
    pub fn claude_denied_tools(&self) -> Vec<String> {
        self.denied_prefixes
            .iter()
            .map(|prefix| format!("Bash({prefix} *)"))
            .collect()
    }

    /// 提示词禁则块（追加在 worker 指令尾部，代码所有）。
    #[must_use]
    pub fn prompt_block(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut lines = vec![
            "【命令禁则（平台管控；每次执行命令前自检，命中即换手段，不要尝试绕过）】"
                .to_string(),
            "以下命令及其等价形态（换参数顺序、加 sudo、改写路径、用脚本包裹都算）禁止执行："
                .to_string(),
        ];
        for prefix in &self.denied_prefixes {
            lines.push(format!("- {prefix} …"));
        }
        lines.extend(PROMPT_ONLY_RULES.iter().map(|rule| format!("- {rule}")));
        lines.push(
            "正常审计操作（探测、请求、脚本、读写任务工作目录）不受限；拿不准某条命令是否"
                .to_string(),
        );
        lines.push("命中禁则时，跳过它并在输出里说明跳过了什么、为什么。".to_string());
        lines.join("\n")
    }
}

/// 命令行归一化：小写 + 空白折叠（前缀比对用）。
fn normalize_command(command_line: &str) -> String {
    command_line.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_covers_destructive_commands() {
        let policy = CommandPolicy::seed();
        assert!(!policy.is_empty());
        for expected in ["rm -rf /", "shutdown", "mkfs", "format", "kill -9 -1"] {
            assert!(
                policy.denied_prefixes.iter().any(|p| p == expected),
                "种子必须包含 {expected}"
            );
        }
    }

    #[test]
    fn violation_matches_prefix_case_and_spacing_insensitive() {
        let policy = CommandPolicy::seed();
        assert_eq!(policy.violation("RM -RF /"), Some("rm -rf /"));
        assert_eq!(policy.violation("rm   -rf   /  --no-preserve-root"), Some("rm -rf /"));
        assert_eq!(policy.violation("shutdown -h now"), Some("shutdown"));
        assert_eq!(policy.violation("format C:"), Some("format"));
    }

    #[test]
    fn violation_does_not_match_word_prefixes() {
        let policy = CommandPolicy::seed();
        // "format" 是禁则词，但 formatted_output.txt 是正常文件名。
        assert_eq!(policy.violation("cat formatted_output.txt"), None);
        assert_eq!(policy.violation("curl https://t.example"), None);
        assert_eq!(policy.violation("python scan.py --target t.example"), None);
    }

    #[test]
    fn from_config_defaults_to_seed_and_honors_disable() {
        let empty = Map::new();
        assert_eq!(CommandPolicy::from_config(&empty), CommandPolicy::seed());

        let off: Map<String, Value> = serde_json::from_str(r#"{"worker_command_policy":{"enabled":false}}"#)
            .expect("合法 JSON");
        assert!(CommandPolicy::from_config(&off).is_empty());

        let custom: Map<String, Value> = serde_json::from_str(
            r#"{"worker_command_policy":{"denied_prefixes":["custom-bad ","  "]}}"#,
        )
        .expect("合法 JSON");
        assert_eq!(
            CommandPolicy::from_config(&custom).denied_prefixes,
            vec!["custom-bad".to_string()]
        );
    }

    #[test]
    fn claude_denied_tools_render_bash_patterns() {
        let policy = CommandPolicy {
            denied_prefixes: vec!["rm -rf /".to_string(), "shutdown".to_string()],
        };
        assert_eq!(
            policy.claude_denied_tools(),
            vec!["Bash(rm -rf / *)".to_string(), "Bash(shutdown *)".to_string()]
        );
    }

    #[test]
    fn prompt_block_lists_prefixes_and_prompt_only_rules() {
        let policy = CommandPolicy::seed();
        let block = policy.prompt_block();
        assert!(block.contains("命令禁则"));
        assert!(block.contains("- rm -rf /"));
        assert!(block.contains("DROP DATABASE"), "SQL 类禁则必须在提示词层");
        assert!(block.contains("FLUSHALL"));
        assert!(CommandPolicy::default().prompt_block().is_empty());
    }
}
