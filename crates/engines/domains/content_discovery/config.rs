use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use models::Project;
use serde_json::{Map, Value};
use url::Url;

const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_THREADS: u64 = 20;
const DEFAULT_RATE: u64 = 50;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("content_discovery configuration must be an object")]
    InvalidSection,
    #[error("content_discovery.engines must be a list of strings")]
    InvalidEngines,
    #[error("content_discovery unsupported engine(s): {0}")]
    UnsupportedEngines(String),
    #[error("content_discovery.target is required")]
    MissingTarget,
    #[error("{tool}: '{field}' {reason}")]
    InvalidField {
        tool: &'static str,
        field: &'static str,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ContentEngine {
    Ffuf,
    Feroxbuster,
    Gobuster,
}

impl ContentEngine {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Ffuf => "ffuf",
            Self::Feroxbuster => "feroxbuster",
            Self::Gobuster => "gobuster",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ffuf" => Some(Self::Ffuf),
            "feroxbuster" => Some(Self::Feroxbuster),
            "gobuster" => Some(Self::Gobuster),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // 字段仅构造（validate 复用构建路径）；执行已移交 catalog + lynceus-mcp broker
pub(crate) struct ToolPlan {
    pub(crate) target: String,
    pub(crate) args: Vec<String>,
    pub(crate) timeout_seconds: u64,
}

/// 为全部声明引擎构建执行计划（validate 复用同一构造路径）。
fn build_plans(
    section: &Map<String, Value>,
    targets: &[String],
    overrides: &HashMap<String, Map<String, Value>>,
) -> Result<Vec<ToolPlan>, ConfigError> {
    let declared = engines(section)?;
    let mut tools = Vec::with_capacity(targets.len().saturating_mul(declared.len()));
    for target in targets {
        for engine in &declared {
            tools.push(tool_plan(
                *engine,
                section,
                target,
                overrides.get(engine.name()),
            )?);
        }
    }
    Ok(tools)
}

/// 目录配置（catalog settings.params）兜底层：每个引擎读取自己已存的
/// settings。mission config 键永远优先；此处只提供缺 key 时的兜底值。
fn catalog_overrides() -> HashMap<String, Map<String, Value>> {
    [
        ContentEngine::Ffuf,
        ContentEngine::Feroxbuster,
        ContentEngine::Gobuster,
    ]
    .into_iter()
    .map(|engine| {
        (
            engine.name().to_string(),
            crate::tool_settings::runtime_overrides(engine.name()).params,
        )
    })
    .collect()
}


pub(super) fn validate(
    config: &Map<String, Value>,
    project: Option<&Project>,
) -> Result<(), ConfigError> {
    let Some(section) = content_section(config)? else {
        return Ok(());
    };
    let targets = configured_targets(&section)?
        .or_else(|| {
            project.and_then(|project| {
                targets_from_pairs(["url", "target", "base_url"], |key| project.target.get(key))
            })
        })
        .ok_or(ConfigError::MissingTarget)?;
    // 校验与构建使用同一兜底层：settings 里已配置的 wordlist 等与 mission
    // config 一样参与校验，避免“校验拒绝、执行可行”的分裂。
    build_plans(&section, &targets, &catalog_overrides()).map(|_| ())
}

fn content_section(config: &Map<String, Value>) -> Result<Option<Map<String, Value>>, ConfigError> {
    match config.get("content_discovery") {
        None => Ok(None),
        Some(Value::Object(section)) => Ok(Some(section.clone())),
        Some(_) => Err(ConfigError::InvalidSection),
    }
}

fn engines(section: &Map<String, Value>) -> Result<Vec<ContentEngine>, ConfigError> {
    let Some(raw) = section.get("engines") else {
        return Ok(vec![
            ContentEngine::Ffuf,
            ContentEngine::Feroxbuster,
            ContentEngine::Gobuster,
        ]);
    };
    let Value::Array(items) = raw else {
        return Err(ConfigError::InvalidEngines);
    };
    let mut parsed = Vec::new();
    let mut unsupported = BTreeSet::new();
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(ConfigError::InvalidEngines);
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if let Some(engine) = ContentEngine::parse(name) {
            parsed.push(engine);
        } else {
            unsupported.insert(name.to_ascii_lowercase());
        }
    }
    if !unsupported.is_empty() {
        return Err(ConfigError::UnsupportedEngines(
            unsupported.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    if parsed.is_empty() {
        parsed.push(ContentEngine::Ffuf);
    }
    Ok(parsed)
}

fn configured_targets(section: &Map<String, Value>) -> Result<Option<Vec<String>>, ConfigError> {
    let raw = section
        .get("targets")
        .filter(|value| !empty_collection_or_string(value))
        .or_else(|| section.get("target"));
    raw.map(|value| string_list(value, "target", "content_discovery"))
        .transpose()
        .map(|targets| targets.filter(|targets| !targets.is_empty()))
}


fn targets_from_pairs<'a>(
    keys: [&str; 3],
    lookup: impl Fn(&str) -> Option<&'a str>,
) -> Option<Vec<String>> {
    keys.into_iter().find_map(|key| {
        lookup(key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| vec![value.to_string()])
    })
}

fn tool_plan(
    engine: ContentEngine,
    section: &Map<String, Value>,
    target: &str,
    catalog: Option<&Map<String, Value>>,
) -> Result<ToolPlan, ConfigError> {
    let local = section
        .get(engine.name())
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    tool_plan_with_local(engine, section, &local, target, catalog)
}

/// 与 [`tool_plan`] 相同，但引擎局部配置层由调用方注入（harness adapter
/// 用它叠加模型 arguments；mission config 局部段仍由调用方先行合并）。
fn tool_plan_with_local(
    engine: ContentEngine,
    section: &Map<String, Value>,
    local: &Map<String, Value>,
    target: &str,
    catalog: Option<&Map<String, Value>>,
) -> Result<ToolPlan, ConfigError> {
    validate_http_target(engine, target)?;
    let view = ConfigView {
        section,
        local: local.clone(),
        catalog,
    };
    let wordlist = wordlist_path(engine, view.get("wordlist"))?;
    let timeout_seconds = positive_integer(
        engine,
        "timeout_seconds",
        view.get("timeout_seconds"),
        DEFAULT_TIMEOUT_SECONDS,
        None,
    )?;
    let threads = positive_integer(
        engine,
        "threads",
        view.get("threads").or_else(|| view.get("concurrency")),
        DEFAULT_THREADS,
        Some(100),
    )?;
    let rate = if matches!(engine, ContentEngine::Gobuster) {
        None
    } else {
        Some(positive_integer(
            engine,
            "rate",
            view.get("rate"),
            DEFAULT_RATE,
            Some(500),
        )?)
    };
    let extensions = extensions(engine, view.get("extensions"))?;
    let status_codes = code_list(engine, "status_codes", view.get("status_codes"))?;
    let filter_status = code_list(engine, "filter_status", view.get("filter_status"))?;
    // 受信 program 由调用方（harness adapter）从 catalog 检测取得；
    // 计划只负责参数面与目标（config 内 executable 解析已退役）。
    let args = build_args(
        engine,
        target,
        &wordlist,
        timeout_seconds,
        threads,
        rate,
        &extensions,
        status_codes.as_deref(),
        filter_status.as_deref(),
    );
    Ok(ToolPlan {
        target: target.to_string(),
        args,
        timeout_seconds,
    })
}

struct ConfigView<'a> {
    section: &'a Map<String, Value>,
    local: Map<String, Value>,
    catalog: Option<&'a Map<String, Value>>,
}

impl ConfigView<'_> {
    /// 解析顺序：引擎局部配置 → `content_discovery` 全局段 → catalog
    /// settings 兜底。前两层是 mission config（永远优先）；catalog 只
    /// 在前两层都缺 key 时生效。
    fn get(&self, key: &str) -> Option<&Value> {
        self.local
            .get(key)
            .or_else(|| self.section.get(key))
            .or_else(|| self.catalog.and_then(|catalog| catalog.get(key)))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_args(
    engine: ContentEngine,
    target: &str,
    wordlist: &Path,
    timeout_seconds: u64,
    threads: u64,
    rate: Option<u64>,
    extensions: &[String],
    status_codes: Option<&str>,
    filter_status: Option<&str>,
) -> Vec<String> {
    let wordlist = wordlist.to_string_lossy().into_owned();
    let mut args = match engine {
        ContentEngine::Ffuf => vec![
            "-json".to_string(),
            "-noninteractive".to_string(),
            "-w".to_string(),
            wordlist,
            "-u".to_string(),
            ffuf_url(target),
            "-t".to_string(),
            threads.to_string(),
            "-rate".to_string(),
            rate.unwrap_or(DEFAULT_RATE).to_string(),
            "-maxtime".to_string(),
            timeout_seconds.to_string(),
        ],
        ContentEngine::Feroxbuster => vec![
            "--url".to_string(),
            target.to_string(),
            "--wordlist".to_string(),
            wordlist,
            "--json".to_string(),
            "--silent".to_string(),
            "--threads".to_string(),
            threads.to_string(),
            "--rate-limit".to_string(),
            rate.unwrap_or(DEFAULT_RATE).to_string(),
            "--time-limit".to_string(),
            format!("{timeout_seconds}s"),
        ],
        ContentEngine::Gobuster => vec![
            "dir".to_string(),
            "--no-color".to_string(),
            "--url".to_string(),
            target.to_string(),
            "--wordlist".to_string(),
            wordlist,
            "--threads".to_string(),
            threads.to_string(),
        ],
    };
    append_optional_args(engine, &mut args, extensions, status_codes, filter_status);
    args
}

fn append_optional_args(
    engine: ContentEngine,
    args: &mut Vec<String>,
    extensions: &[String],
    status_codes: Option<&str>,
    filter_status: Option<&str>,
) {
    if !extensions.is_empty() {
        args.push(
            match engine {
                ContentEngine::Ffuf => "-e",
                ContentEngine::Feroxbuster | ContentEngine::Gobuster => "--extensions",
            }
            .to_string(),
        );
        args.push(extensions.join(","));
    }
    if let Some(codes) = status_codes {
        args.push(
            match engine {
                ContentEngine::Ffuf => "-mc",
                ContentEngine::Feroxbuster | ContentEngine::Gobuster => "--status-codes",
            }
            .to_string(),
        );
        args.push(codes.to_string());
        if matches!(engine, ContentEngine::Gobuster) {
            args.extend(["--status-codes-blacklist".to_string(), String::new()]);
        }
    }
    if let Some(codes) = filter_status {
        args.push(
            match engine {
                ContentEngine::Ffuf => "-fc",
                ContentEngine::Feroxbuster => "--filter-status",
                ContentEngine::Gobuster => "--status-codes-blacklist",
            }
            .to_string(),
        );
        args.push(codes.to_string());
    }
}

fn validate_http_target(engine: ContentEngine, target: &str) -> Result<(), ConfigError> {
    let valid = Url::parse(target)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
    if valid {
        Ok(())
    } else {
        Err(invalid(
            engine,
            "target",
            "must be an http:// or https:// URL",
        ))
    }
}

fn wordlist_path(engine: ContentEngine, raw: Option<&Value>) -> Result<PathBuf, ConfigError> {
    let path = match raw {
        Some(value) if value.as_str().is_some_and(|text| !text.trim().is_empty()) => {
            PathBuf::from(non_empty_string(value, "wordlist", engine.name())?)
        }
        Some(_) | None => bundled_wordlist().ok_or_else(|| {
            invalid(
                engine,
                "wordlist",
                "is required because no bundled wordlist was found",
            )
        })?,
    };
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid(
            engine,
            "wordlist",
            "must not contain current or parent segments",
        ));
    }
    Ok(path)
}

fn bundled_wordlist() -> Option<PathBuf> {
    let raw = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("resources/wordlists/common.txt");
    // 受信构建期路径：canonicalize 消解 .. 段，使其能通过 wordlist_path
    // 的目录穿越检查（用户提供的路径仍按原样拒绝 ..）。
    raw.canonicalize().ok().filter(|path| path.is_file())
}

fn positive_integer(
    engine: ContentEngine,
    field: &'static str,
    raw: Option<&Value>,
    default: u64,
    maximum: Option<u64>,
) -> Result<u64, ConfigError> {
    let value = raw
        .map_or(Some(default), Value::as_u64)
        .ok_or_else(|| invalid(engine, field, "must be a positive integer"))?;
    if value == 0 {
        return Err(invalid(engine, field, "must be a positive integer"));
    }
    if maximum.is_some_and(|maximum| value > maximum) {
        return Err(invalid(
            engine,
            field,
            &format!("must be <= {}", maximum.unwrap_or_default()),
        ));
    }
    Ok(value)
}

fn extensions(engine: ContentEngine, raw: Option<&Value>) -> Result<Vec<String>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    string_list(raw, "extensions", engine.name())?
        .into_iter()
        .map(|value| {
            let extension = value.trim().trim_start_matches('.').to_string();
            if !extension.is_empty()
                && extension.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "._+-".contains(character)
                })
            {
                Ok(extension)
            } else {
                Err(invalid(
                    engine,
                    "extensions",
                    "entries must be safe extensions",
                ))
            }
        })
        .collect()
}

fn code_list(
    engine: ContentEngine,
    field: &'static str,
    raw: Option<&Value>,
) -> Result<Option<String>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let value = match raw {
        // 数字数组是 mission config 的既有形态；字符串项兼容 catalog
        // settings 的 string_list 配置（与字符串标量同一字符集约束）。
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Number(_) => item
                    .as_i64()
                    .map(|number| number.to_string())
                    .ok_or_else(|| invalid(engine, field, "entries must be integers")),
                Value::String(text) if is_code_text(text) => Ok(text.trim().to_string()),
                _ => Err(invalid(
                    engine,
                    field,
                    "entries must be integers or digit/range strings",
                )),
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(","),
        Value::String(text) if is_code_text(text) => text.trim().to_string(),
        Value::String(_) => {
            return Err(invalid(
                engine,
                field,
                "must contain only digits, comma, or ranges",
            ));
        }
        _ => return Err(invalid(engine, field, "must be a string or integer list")),
    };
    Ok(Some(value))
}

/// `200`、`200,301`、`200-299` 这类状态码文本的统一字符集判定。
fn is_code_text(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|character| character.is_ascii_digit() || matches!(character, ',' | '-'))
}

fn string_list(
    raw: &Value,
    field: &'static str,
    tool: &'static str,
) -> Result<Vec<String>, ConfigError> {
    match raw {
        Value::String(value) if !value.trim().is_empty() => Ok(vec![value.trim().to_string()]),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| ConfigError::InvalidField {
                        tool,
                        field,
                        reason: "entries must be non-empty strings".to_string(),
                    })
            })
            .collect(),
        _ => Err(ConfigError::InvalidField {
            tool,
            field,
            reason: "must be a string or list of strings".to_string(),
        }),
    }
}

fn non_empty_string(
    raw: &Value,
    field: &'static str,
    tool: &'static str,
) -> Result<String, ConfigError> {
    raw.as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ConfigError::InvalidField {
            tool,
            field,
            reason: "is required and must be a non-empty string".to_string(),
        })
}

fn invalid(engine: ContentEngine, field: &'static str, reason: &str) -> ConfigError {
    ConfigError::InvalidField {
        tool: engine.name(),
        field,
        reason: reason.to_string(),
    }
}

fn empty_collection_or_string(value: &Value) -> bool {
    matches!(value, Value::Array(items) if items.is_empty())
        || matches!(value, Value::String(text) if text.is_empty())
}

fn ffuf_url(target: &str) -> String {
    if target.contains("FUZZ") {
        target.to_string()
    } else {
        format!("{}/FUZZ", target.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn section() -> Map<String, Value> {
        json!({
            "target": "https://example.test",
            "wordlist": "D:/lists/common.txt",
            "threads": 8,
            "rate": 30,
            "timeout_seconds": 120,
            "extensions": ["php", "json"],
            "status_codes": [200, 401],
            "filter_status": "404,500"
        })
        .as_object()
        .cloned()
        .expect("test configuration is an object")
    }

    #[test]
    fn builds_fixed_argv_for_all_supported_tools() {
        let section = section();
        let ffuf = tool_plan(ContentEngine::Ffuf, &section, "https://example.test", None)
            .expect("valid ffuf plan");
        assert_eq!(
            &ffuf.args[..8],
            [
                "-json",
                "-noninteractive",
                "-w",
                "D:/lists/common.txt",
                "-u",
                "https://example.test/FUZZ",
                "-t",
                "8"
            ]
        );
        assert_eq!(ffuf.timeout_seconds, 120);
        assert!(ffuf.args.windows(2).any(|pair| pair == ["-e", "php,json"]));

        let ferox = tool_plan(
            ContentEngine::Feroxbuster,
            &section,
            "https://example.test",
            None,
        )
        .expect("valid feroxbuster plan");
        assert_eq!(
            &ferox.args[..8],
            [
                "--url",
                "https://example.test",
                "--wordlist",
                "D:/lists/common.txt",
                "--json",
                "--silent",
                "--threads",
                "8"
            ]
        );

        let gobuster = tool_plan(
            ContentEngine::Gobuster,
            &section,
            "https://example.test",
            None,
        )
        .expect("valid gobuster plan");
        assert_eq!(
            &gobuster.args[..8],
            [
                "dir",
                "--no-color",
                "--url",
                "https://example.test",
                "--wordlist",
                "D:/lists/common.txt",
                "--threads",
                "8"
            ]
        );
    }

    #[test]
    fn rejects_unsafe_or_unbounded_configuration() {
        let mut section = section();
        assert!(tool_plan(ContentEngine::Ffuf, &section, "ftp://example.test", None).is_err());
        section.insert("threads".to_string(), Value::Number(500.into()));
        assert!(
            tool_plan(
                ContentEngine::Gobuster,
                &section,
                "https://example.test",
                None
            )
            .is_err()
        );
        section.insert(
            "wordlist".to_string(),
            Value::String("../secrets.txt".to_string()),
        );
        assert!(tool_plan(ContentEngine::Ffuf, &section, "https://example.test", None).is_err());
    }

    #[test]
    fn catalog_settings_fill_missing_mission_keys_and_yield_to_mission() {
        let section = json!({"target": "https://example.test"})
            .as_object()
            .cloned()
            .expect("test configuration is an object");
        let catalog = json!({
            "wordlist": "D:/lists/catalog.txt",
            "threads": 12,
            "status_codes": ["200", "301"],
            "extensions": ["php"]
        })
        .as_object()
        .cloned()
        .expect("catalog settings is an object");
        let ffuf = tool_plan(
            ContentEngine::Ffuf,
            &section,
            "https://example.test",
            Some(&catalog),
        )
        .expect("catalog fallback must produce a valid plan");
        assert!(
            ffuf.args
                .windows(2)
                .any(|pair| pair == ["-w", "D:/lists/catalog.txt"])
        );
        assert!(ffuf.args.windows(2).any(|pair| pair == ["-t", "12"]));
        assert!(ffuf.args.windows(2).any(|pair| pair == ["-mc", "200,301"]));
        assert!(ffuf.args.windows(2).any(|pair| pair == ["-e", "php"]));

        // mission 显式提供的键永远优先于 catalog settings。
        let mission = json!({
            "target": "https://example.test",
            "wordlist": "D:/lists/mission.txt"
        })
        .as_object()
        .cloned()
        .expect("test configuration is an object");
        let ffuf = tool_plan(
            ContentEngine::Ffuf,
            &mission,
            "https://example.test",
            Some(&catalog),
        )
        .expect("mission priority plan");
        assert!(
            ffuf.args
                .windows(2)
                .any(|pair| pair == ["-w", "D:/lists/mission.txt"])
        );
        assert!(!ffuf.args.contains(&"D:/lists/catalog.txt".to_string()));
    }

    #[test]
    fn catalog_settings_reject_invalid_status_code_entries() {
        let section = json!({"target": "https://example.test"})
            .as_object()
            .cloned()
            .expect("test configuration is an object");
        let catalog = json!({
            "wordlist": "D:/lists/catalog.txt",
            "status_codes": ["200", "oops"]
        })
        .as_object()
        .cloned()
        .expect("catalog settings is an object");
        let error = tool_plan(
            ContentEngine::Ffuf,
            &section,
            "https://example.test",
            Some(&catalog),
        )
        .expect_err("non-numeric catalog status codes must fail");
        assert!(error.to_string().contains("status_codes"));
    }
}
