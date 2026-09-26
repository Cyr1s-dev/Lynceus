//! Invocation settings for catalog tools: validation, persistence, sanitized
//! echo views and runtime overrides.
//!
//! Settings live in the `settings` field of a local-tools.json entry:
//! `{"params": {...}, "env": {...}}`. Configure semantics are frozen by the
//! invocation-config design contract:
//! - params are a **full replacement** validated against the declared
//!   invocation spec (unknown key / kind mismatch / integer range / blank
//!   string / empty `string_list` item / size caps are rejected with 422);
//! - env values **merge** per key (`null` or `""` clears; keys absent from
//!   the request keep their stored value) because the API never echoes env
//!   plaintext, so clients cannot resubmit values they cannot see;
//! - a tool without an invocation spec rejects params/env entirely;
//! - settings cannot attach to a tool that has no local-tools.json entry
//!   (an entry is only created together with an `executable_path`).
//! 【统一工具系统核心】工具参数 schema 校验与本地 settings（env/params）。
//! `tool_execute` broker 守卫链复用 `validate_params`。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::tool_catalog::{
    CatalogTool, ToolCatalogError, ToolConfiguredSettings, ToolConfiguredSettingsView,
    ToolEnvKeySpec, ToolInvocationSpec, ToolParamKind, ToolParamSpec, load_local_tools,
    local_tools_config_path,
};
use crate::tool_gateway::ToolRequest;

/// Maximum configured params per tool.
pub const MAX_PARAMS_PER_TOOL: usize = 64;
/// Maximum configured env keys per tool (per request and after merge).
pub const MAX_ENV_KEYS_PER_TOOL: usize = 64;
/// Maximum items in one configured `string_list` param.
pub const MAX_STRING_LIST_ITEMS: usize = 64;

fn invalid_config(message: impl Into<String>) -> ToolCatalogError {
    ToolCatalogError::InvalidConfig(message.into())
}

// ---------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------

/// Validate a full params replacement against the declared invocation spec.
///
/// # Errors
/// [`ToolCatalogError::InvalidConfig`] on unknown key, kind mismatch,
/// integer outside the declared `minimum`/`maximum`, blank string, empty
/// `string_list` item, or more than [`MAX_PARAMS_PER_TOOL`] entries.
pub fn validate_params(
    spec: &ToolInvocationSpec,
    incoming: &Map<String, Value>,
) -> Result<(), ToolCatalogError> {
    if incoming.len() > MAX_PARAMS_PER_TOOL {
        return Err(invalid_config(format!(
            "params accept at most {MAX_PARAMS_PER_TOOL} entries per tool"
        )));
    }
    for (key, value) in incoming {
        // `target` 是 `build_argv` 的通用目标注入键（catalog 用 target_flag
        // 声明怎么传），不属于任何工具的 invocation.params。放行它，否则
        // worker 一旦按 schema 广告传 target 就被 "unknown param" 拒掉。
        if key == "target" {
            match value {
                Value::String(text) if !text.trim().is_empty() => {}
                _ => {
                    return Err(invalid_config(
                        "param \"target\": must be a non-empty string".to_string(),
                    ));
                }
            }
            continue;
        }
        let Some(declared) = spec.params.iter().find(|param| param.key == *key) else {
            let declared_keys = spec
                .params
                .iter()
                .map(|param| param.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid_config(format!(
                "unknown param {key:?}; this tool declares: [{declared_keys}]"
            )));
        };
        if let Some(reason) = param_kind_violation(declared, value) {
            return Err(invalid_config(format!("param {key:?}: {reason}")));
        }
    }
    Ok(())
}

fn param_kind_violation(declared: &ToolParamSpec, value: &Value) -> Option<String> {
    match declared.kind {
        ToolParamKind::String | ToolParamKind::Path => match value {
            Value::String(text) if !text.trim().is_empty() => None,
            Value::String(_) => Some("must be a non-empty string".to_string()),
            _ => Some("must be a string".to_string()),
        },
        ToolParamKind::Integer => {
            let Some(number) = value.as_i64() else {
                return Some("must be an integer".to_string());
            };
            if let Some(minimum) = declared.minimum
                && number < minimum
            {
                return Some(format!("must be >= {minimum}"));
            }
            if let Some(maximum) = declared.maximum
                && number > maximum
            {
                return Some(format!("must be <= {maximum}"));
            }
            None
        }
        ToolParamKind::Boolean => (!value.is_boolean()).then(|| "must be a boolean".to_string()),
        ToolParamKind::StringList => string_list_violation(value),
    }
}

fn string_list_violation(value: &Value) -> Option<String> {
    let Value::Array(items) = value else {
        return Some("must be an array of strings".to_string());
    };
    if items.len() > MAX_STRING_LIST_ITEMS {
        return Some(format!(
            "must contain at most {MAX_STRING_LIST_ITEMS} items"
        ));
    }
    for item in items {
        match item {
            Value::String(text) if !text.trim().is_empty() => {}
            Value::String(_) => return Some("entries must be non-empty strings".to_string()),
            _ => return Some("entries must be strings".to_string()),
        }
    }
    None
}

/// Merge an env update into the stored env values.
///
/// Merge semantics: keys absent from `incoming` keep their stored value;
/// `null` or an empty string clears the key; provided values are stored
/// verbatim (never trimmed). Unknown keys - not declared in the spec's
/// `env_keys` allowlist - are rejected. Plaintext values never appear in
/// error messages.
///
/// # Errors
/// [`ToolCatalogError::InvalidConfig`] on unknown key or size-cap overrun.
pub fn merge_env(
    spec: &ToolInvocationSpec,
    existing: &BTreeMap<String, String>,
    incoming: &BTreeMap<String, Option<String>>,
) -> Result<BTreeMap<String, String>, ToolCatalogError> {
    if incoming.len() > MAX_ENV_KEYS_PER_TOOL {
        return Err(invalid_config(format!(
            "env accepts at most {MAX_ENV_KEYS_PER_TOOL} keys per request"
        )));
    }
    let mut merged = existing.clone();
    for (name, value) in incoming {
        if !spec
            .env_keys
            .iter()
            .any(|declared: &ToolEnvKeySpec| declared.name == *name)
        {
            let declared_names = spec
                .env_keys
                .iter()
                .map(|key| key.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid_config(format!(
                "unknown env key {name:?}; this tool declares: [{declared_names}]"
            )));
        }
        match value {
            None => {
                merged.remove(name);
            }
            Some(text) if text.is_empty() => {
                merged.remove(name);
            }
            Some(text) => {
                merged.insert(name.clone(), text.clone());
            }
        }
    }
    if merged.len() > MAX_ENV_KEYS_PER_TOOL {
        return Err(invalid_config(format!(
            "env accepts at most {MAX_ENV_KEYS_PER_TOOL} configured keys per tool"
        )));
    }
    Ok(merged)
}

// ---------------------------------------------------------------------------
// persistence
// ---------------------------------------------------------------------------

/// Read configured settings from local-tools.json, keyed by lowercased
/// `tool_name`/`name` - the same matching rule as executable detection.
/// Malformed files, entries and settings blobs are treated as absent.
#[must_use]
pub fn load_local_tool_settings(path: &Path) -> HashMap<String, ToolConfiguredSettings> {
    let mut settings = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return settings;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&text) else {
        return settings;
    };
    let Some(items) = payload
        .as_object()
        .and_then(|object| object.get("local_tools"))
        .and_then(Value::as_array)
    else {
        return settings;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(raw) = object.get("settings") else {
            continue;
        };
        let Ok(parsed) = serde_json::from_value::<ToolConfiguredSettings>(raw.clone()) else {
            continue;
        };
        for key in ["tool_name", "name"] {
            if let Some(name) = object
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                settings.insert(name.to_lowercase(), parsed.clone());
            }
        }
    }
    settings
}

/// Write settings for one tool into local-tools.json, preserving every other
/// field of the matched entry and the rest of the file. Empty settings (or
/// `None`) remove the `settings` key from the entry.
///
/// The entry is matched by `tool_name` equal to the catalog id
/// (case-insensitively) - the shape every writer in this crate produces.
///
/// # Errors
/// [`ToolCatalogError::InvalidConfig`] when no entry matches the tool;
/// [`ToolCatalogError::ConfigIo`] / [`ToolCatalogError::ConfigJson`] on write
/// or encoding failure.
pub fn store_local_tool_settings(
    tool_id: &str,
    settings: Option<&ToolConfiguredSettings>,
    path: &Path,
) -> Result<(), ToolCatalogError> {
    let mut payload = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"local_tools": []}));
    let items = payload
        .as_object_mut()
        .and_then(|object| object.get_mut("local_tools"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            ToolCatalogError::InvalidCatalog("local_tools must be an array".to_string())
        })?;
    let normalized = tool_id.to_lowercase();
    let entry = items
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .find(|item| {
            item.get("tool_name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.trim().eq_ignore_ascii_case(&normalized))
        })
        .ok_or_else(|| {
            invalid_config(format!(
                "tool {tool_id:?} has no local-tools.json entry; provide executable_path to \
                 create one before storing settings"
            ))
        })?;
    match settings {
        Some(value) if !value.params.is_empty() || !value.env.is_empty() => {
            entry.insert("settings".to_string(), serde_json::to_value(value)?);
        }
        _ => {
            entry.remove("settings");
        }
    }
    let text = serde_json::to_string_pretty(&payload)?;
    std::fs::write(path, text).map_err(|source| ToolCatalogError::ConfigIo {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// configure entry point
// ---------------------------------------------------------------------------

/// Resolve and preflight the invocation settings target for a configure
/// request that carries params/env.
///
/// Returns the declared invocation spec after verifying that the tool
/// declares one and that a settings target exists: either a local-tools.json
/// entry is already present, or the request provides `executable_path` to
/// create one in the same request.
///
/// # Errors
/// [`ToolCatalogError::InvalidConfig`] when the tool has no invocation spec,
/// or when neither an existing entry nor an executable path is available.
pub fn ensure_settings_target<'a>(
    tool: &'a CatalogTool,
    executable_path: Option<&'a str>,
    local_tools_path: &'a Path,
) -> Result<&'a ToolInvocationSpec, ToolCatalogError> {
    let invocation = tool.invocation.as_ref().ok_or_else(|| {
        invalid_config(format!(
            "tool {:?} does not support invocation configuration",
            tool.id
        ))
    })?;
    let has_entry = load_local_tools(local_tools_path).contains_key(&tool.id.to_lowercase());
    let provides_executable = executable_path
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !has_entry && !provides_executable {
        return Err(invalid_config(format!(
            "tool {:?} has no local-tools.json entry; provide executable_path to create one \
             before storing settings",
            tool.id
        )));
    }
    Ok(invocation)
}

/// Validate and persist one configure request's params/env for a tool.
///
/// Params are a full replacement; env merges into stored values; a section
/// absent from the request keeps its stored value. The merged result is
/// returned so callers can use it without re-reading the file.
///
/// # Errors
/// [`ToolCatalogError::InvalidConfig`] on any validation violation or when
/// the tool has no local-tools.json entry; I/O errors on persistence.
pub fn apply_tool_settings(
    tool: &CatalogTool,
    params: Option<&Map<String, Value>>,
    env: Option<&BTreeMap<String, Option<String>>>,
    path: &Path,
) -> Result<ToolConfiguredSettings, ToolCatalogError> {
    let invocation = tool.invocation.as_ref().ok_or_else(|| {
        invalid_config(format!(
            "tool {:?} does not support invocation configuration",
            tool.id
        ))
    })?;
    let existing = load_local_tool_settings(path);
    let stored = existing
        .get(&tool.id.to_lowercase())
        .or_else(|| existing.get(&tool.name.to_lowercase()))
        .cloned()
        .unwrap_or_default();
    let next_params = match params {
        Some(incoming) => {
            validate_params(invocation, incoming)?;
            incoming.clone()
        }
        None => stored.params.clone(),
    };
    let next_env = match env {
        Some(incoming) => merge_env(invocation, &stored.env, incoming)?,
        None => stored.env.clone(),
    };
    let merged = ToolConfiguredSettings {
        params: next_params,
        env: next_env,
    };
    store_local_tool_settings(&tool.id, Some(&merged), path)?;
    Ok(merged)
}

// ---------------------------------------------------------------------------
// sanitized echo
// ---------------------------------------------------------------------------

/// Build the sanitized response view for stored settings: params verbatim,
/// env reduced to the sorted list of configured key names. `None` when
/// nothing is configured (settings all empty).
#[must_use]
pub fn configured_view(settings: &ToolConfiguredSettings) -> Option<ToolConfiguredSettingsView> {
    if settings.params.is_empty() && settings.env.is_empty() {
        return None;
    }
    let mut env_set: Vec<String> = settings.env.keys().cloned().collect();
    env_set.sort();
    Some(ToolConfiguredSettingsView {
        params: settings.params.clone(),
        env_set,
    })
}

// ---------------------------------------------------------------------------
// runtime overrides (solver / adapter consumption)
// ---------------------------------------------------------------------------

/// Persisted invocation overrides resolved for one tool at solve time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRuntimeOverrides {
    /// Configured params; adapters consult these only when the mission
    /// config provides no value for a key.
    pub params: Map<String, Value>,
    /// Configured env values, merged into the tool's subprocess environment.
    pub env: BTreeMap<String, String>,
}

/// Resolve persisted overrides for one tool from the default
/// local-tools.json path ([`local_tools_config_path`]).
#[must_use]
pub fn runtime_overrides(tool_name: &str) -> ToolRuntimeOverrides {
    let settings = load_local_tool_settings(&local_tools_config_path());
    settings
        .get(&tool_name.to_lowercase())
        .map(|value| ToolRuntimeOverrides {
            params: value.params.clone(),
            env: value.env.clone(),
        })
        .unwrap_or_default()
}

/// Merge persisted env overrides for the request's tool into the request's
/// subprocess environment. Called at [`ToolRequest`] construction sites so
/// every adapter inherits env injection without per-adapter code.
pub fn apply_runtime_env(request: &mut ToolRequest) {
    request
        .env
        .extend(runtime_overrides(&request.tool_name).env);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::tool_catalog::{ToolEnvKeySpec, ToolParamKind, ToolParamSpec, configure_local_tool};

    fn spec(params: &[(&str, ToolParamKind)], env_keys: &[&str]) -> ToolInvocationSpec {
        ToolInvocationSpec {
            params: params
                .iter()
                .map(|(key, kind)| ToolParamSpec {
                    key: (*key).to_string(),
                    kind: *kind,
                    flag: None,
                    default: None,
                    required: false,
                    description: None,
                    minimum: None,
                    maximum: None,
                })
                .collect(),
            env_keys: env_keys
                .iter()
                .map(|name| ToolEnvKeySpec {
                    name: (*name).to_string(),
                    description: None,
                    required: false,
                })
                .collect(),
        }
    }

    fn object(value: &serde_json::Value) -> Map<String, Value> {
        value.as_object().cloned().expect("test value is object")
    }

    #[test]
    fn validate_params_accepts_declared_values() {
        let invocation = spec(
            &[
                ("config", ToolParamKind::String),
                ("rule_path", ToolParamKind::Path),
                ("rate", ToolParamKind::Integer),
                ("headless", ToolParamKind::Boolean),
                ("include", ToolParamKind::StringList),
            ],
            &[],
        );
        validate_params(
            &invocation,
            &object(&json!({
                "config": "p/custom",
                "rule_path": "C:/rules",
                "rate": 5,
                "headless": false,
                "include": ["a.go", "b.py"]
            })),
        )
        .expect("declared values must validate");
        validate_params(&invocation, &object(&json!({}))).expect("empty replacement is valid");
    }

    /// 回归：`target` 是 `build_argv` 的通用目标注入键，不属于任何工具的
    /// `invocation.params`。放行它之前，worker 一旦按 `describe()` 广告传
    /// target 就被 "unknown param" 拒掉——修了 ffuf 缺 `-u` 也会被这道闸
    /// 挡死，所以两边必须同时放行。
    #[test]
    fn validate_params_accepts_the_generic_target_key() {
        let invocation = spec(&[("wordlist", ToolParamKind::Path)], &[]);

        validate_params(
            &invocation,
            &object(&json!({
                "wordlist": "wl.txt",
                "target": "http://target.test/"
            })),
        )
        .expect("target must be accepted even though no tool declares it");

        // 空串/非字符串仍要拒：那会造出 `-u ""` 这种残缺 argv。
        assert!(
            validate_params(&invocation, &object(&json!({"target": "   "}))).is_err(),
            "blank target must be rejected"
        );
        assert!(
            validate_params(&invocation, &object(&json!({"target": 7}))).is_err(),
            "non-string target must be rejected"
        );
    }

    #[test]
    fn validate_params_rejects_unknown_keys_and_kind_mismatches() {
        let invocation = spec(
            &[
                ("config", ToolParamKind::String),
                ("rate", ToolParamKind::Integer),
            ],
            &[],
        );
        let error = validate_params(&invocation, &object(&json!({"unknown": "x"})))
            .expect_err("unknown key must fail");
        assert!(error.to_string().contains("unknown param"));

        let error = validate_params(&invocation, &object(&json!({"config": 7})))
            .expect_err("number for string must fail");
        assert!(error.to_string().contains("must be a string"));

        let error = validate_params(&invocation, &object(&json!({"rate": "fast"})))
            .expect_err("string for integer must fail");
        assert!(error.to_string().contains("must be an integer"));

        let error = validate_params(&invocation, &object(&json!({"rate": true})))
            .expect_err("bool for integer must fail");
        assert!(error.to_string().contains("must be an integer"));

        let error = validate_params(&invocation, &object(&json!({"config": ""})))
            .expect_err("blank string must fail");
        assert!(error.to_string().contains("non-empty"));
    }

    #[test]
    fn validate_params_enforces_integer_bounds_and_list_shape() {
        let invocation = spec(
            &[
                ("rate", ToolParamKind::Integer),
                ("include", ToolParamKind::StringList),
            ],
            &[],
        );
        let mut bounded = invocation.clone();
        bounded.params[0].minimum = Some(1);
        bounded.params[0].maximum = Some(500);
        let error = validate_params(&bounded, &object(&json!({"rate": 0})))
            .expect_err("below minimum must fail");
        assert!(error.to_string().contains("must be >= 1"));
        let error = validate_params(&bounded, &object(&json!({"rate": 501})))
            .expect_err("above maximum must fail");
        assert!(error.to_string().contains("must be <= 500"));
        validate_params(&bounded, &object(&json!({"rate": 500})))
            .expect("boundary value must pass");

        let error = validate_params(&invocation, &object(&json!({"include": ["a", ""]})))
            .expect_err("empty list item must fail");
        assert!(error.to_string().contains("non-empty strings"));
        let error = validate_params(&invocation, &object(&json!({"include": [1]})))
            .expect_err("non-string item must fail");
        assert!(error.to_string().contains("entries must be strings"));

        let many: Vec<String> = (0..=MAX_STRING_LIST_ITEMS)
            .map(|i| format!("v{i}"))
            .collect();
        let error = validate_params(&invocation, &object(&json!({ "include": many })))
            .expect_err("oversized list must fail");
        assert!(error.to_string().contains("at most"));
    }

    #[test]
    fn validate_params_enforces_per_tool_entry_cap() {
        let invocation = spec(&[], &[]);
        let entries: serde_json::Map<String, Value> = (0..=MAX_PARAMS_PER_TOOL)
            .map(|i| (format!("k{i}"), Value::Null))
            .collect();
        let error = validate_params(&invocation, &entries).expect_err("oversized map must fail");
        assert!(error.to_string().contains("at most"));
    }

    #[test]
    fn merge_env_sets_clears_and_keeps_stored_values() {
        let invocation = spec(&[], &["TOKEN_A", "TOKEN_B"]);
        let existing = BTreeMap::from([
            ("TOKEN_A".to_string(), "old-a".to_string()),
            ("TOKEN_B".to_string(), "old-b".to_string()),
        ]);
        let merged = merge_env(
            &invocation,
            &existing,
            &BTreeMap::from([
                ("TOKEN_A".to_string(), Some("new-a".to_string())),
                ("TOKEN_B".to_string(), Some(String::new())),
            ]),
        )
        .expect("declared keys must merge");
        assert_eq!(merged.get("TOKEN_A").map(String::as_str), Some("new-a"));
        assert!(
            !merged.contains_key("TOKEN_B"),
            "empty string clears the key"
        );

        // Keys absent from the request keep their stored value; null clears.
        let merged = merge_env(
            &invocation,
            &BTreeMap::from([("TOKEN_B".to_string(), "kept".to_string())]),
            &BTreeMap::from([("TOKEN_B".to_string(), None)]),
        )
        .expect("null clears a declared key");
        assert!(!merged.contains_key("TOKEN_B"), "null clears the key");

        // Keys absent from the request keep their stored value.
        let merged = merge_env(
            &invocation,
            &BTreeMap::from([("TOKEN_B".to_string(), "kept".to_string())]),
            &BTreeMap::new(),
        )
        .expect("empty update keeps stored values");
        assert_eq!(merged.get("TOKEN_B").map(String::as_str), Some("kept"));

        let error = merge_env(
            &invocation,
            &BTreeMap::new(),
            &BTreeMap::from([("UNDECLARED_KEY".to_string(), Some("v".to_string()))]),
        )
        .expect_err("undeclared env key must fail");
        assert!(error.to_string().contains("unknown env key"));
    }

    fn catalog_tool(id: &str) -> CatalogTool {
        crate::tool_catalog::load_catalog()
            .expect("embedded catalog must load")
            .into_iter()
            .find(|tool| tool.id == id)
            .unwrap_or_else(|| panic!("catalog must contain {id}"))
    }

    #[test]
    fn settings_roundtrip_persists_and_masks_env_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("config").join("local-tools.json");
        configure_local_tool("semgrep", Some(r"C:\Tools\semgrep.exe"), true, &path)
            .expect("entry must persist");
        let semgrep = catalog_tool("semgrep");
        let params = object(&json!({"config": "auto", "timeout_seconds": 60}));
        let env = BTreeMap::from([(
            "SEMGREP_APP_TOKEN".to_string(),
            Some("secret-token-value".to_string()),
        )]);
        apply_tool_settings(&semgrep, Some(&params), Some(&env), &path)
            .expect("settings must apply");

        // Persisted entry: settings coexists with the sibling entry fields.
        let raw = std::fs::read_to_string(&path).expect("config must exist");
        let payload: Value = serde_json::from_str(&raw).expect("config must parse");
        let entry = payload["local_tools"]
            .as_array()
            .expect("local_tools array")
            .iter()
            .find(|item| item["tool_name"] == "semgrep")
            .expect("semgrep entry")
            .clone();
        assert_eq!(entry["executable_path"], json!(r"C:\Tools\semgrep.exe"));
        assert_eq!(entry["enabled"], json!(true));
        assert_eq!(entry["settings"]["params"]["timeout_seconds"], json!(60));

        // Read back and echo: env only as names, plaintext never appears.
        let stored = load_local_tool_settings(&path);
        let view = configured_view(stored.get("semgrep").expect("settings stored"))
            .expect("non-empty settings have a view");
        assert_eq!(view.env_set, ["SEMGREP_APP_TOKEN"]);
        assert_eq!(view.params.get("config"), Some(&json!("auto")));
        let serialized = serde_json::to_string(&view).expect("view must serialize");
        assert!(
            !serialized.contains("secret-token-value"),
            "env plaintext leaked in echo view: {serialized}"
        );

        // Full-replacement semantics: the second params write keeps only
        // the new set.
        let replacement = object(&json!({"timeout_seconds": 90}));
        apply_tool_settings(&semgrep, Some(&replacement), None, &path)
            .expect("replacement must apply");
        let stored = load_local_tool_settings(&path);
        let view = configured_view(stored.get("semgrep").expect("settings stored")).expect("view");
        assert_eq!(view.params.len(), 1);
        assert_eq!(view.params.get("timeout_seconds"), Some(&json!(90)));
    }

    #[test]
    fn env_merges_across_requests_and_clear_removes_from_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("local-tools.json");
        configure_local_tool("semgrep", Some(r"C:\Tools\semgrep.exe"), true, &path)
            .expect("entry must persist");
        let semgrep = catalog_tool("semgrep");
        let env = BTreeMap::from([(
            "SEMGREP_APP_TOKEN".to_string(),
            Some("token-one".to_string()),
        )]);
        apply_tool_settings(&semgrep, None, Some(&env), &path).expect("initial env must apply");

        // Second request updates the key; a later null clears it entirely.
        let update = BTreeMap::from([(
            "SEMGREP_APP_TOKEN".to_string(),
            Some("token-two".to_string()),
        )]);
        apply_tool_settings(&semgrep, None, Some(&update), &path).expect("update must apply");
        let stored = load_local_tool_settings(&path);
        let view = configured_view(stored.get("semgrep").expect("settings stored")).expect("view");
        assert_eq!(view.env_set, ["SEMGREP_APP_TOKEN"]);
        // Once everything is cleared the settings field is removed entirely.
        let clear = BTreeMap::from([("SEMGREP_APP_TOKEN".to_string(), None)]);
        apply_tool_settings(&semgrep, None, Some(&clear), &path).expect("clear must apply");
        let raw = std::fs::read_to_string(&path).expect("config must exist");
        assert!(
            !raw.contains("settings"),
            "empty settings must be removed: {raw}"
        );
    }

    #[test]
    fn settings_require_an_existing_entry_or_executable_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("local-tools.json");
        let semgrep = catalog_tool("semgrep");
        let dnsx = catalog_tool("dnsx"); // declares no invocation spec

        let error = apply_tool_settings(
            &semgrep,
            Some(&object(&json!({"config": "auto"}))),
            None,
            &path,
        )
        .expect_err("no entry must fail");
        assert!(error.to_string().contains("no local-tools.json entry"));

        let error = ensure_settings_target(&semgrep, None, &path)
            .expect_err("no entry and no executable must fail");
        assert!(error.to_string().contains("executable_path"));

        ensure_settings_target(&semgrep, Some("C:/Tools/semgrep.exe"), &path)
            .expect("explicit executable creates the entry in the same request");

        let error = ensure_settings_target(&dnsx, Some("C:/Tools/dnsx.exe"), &path)
            .expect_err("tool without invocation spec must fail");
        assert!(error.to_string().contains("does not support invocation"));
    }
}
