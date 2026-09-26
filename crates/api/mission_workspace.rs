//! Mission workspace filesystem helpers.
//!
//! Mirrors `server/api/mission_workspace.py`: canonical workspace paths under
//! a configurable root, ASCII slugs for user-visible text, standard subdirs,
//! collision-safe upload allocation and workspace metadata propagation.

use std::path::{Path, PathBuf};

use models::{ArtifactRecord, Mission, Timestamp};
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

use engines::upload_intake::safe_upload_filename;

/// 标准 Mission workspace 子目录（Python `MISSION_WORKSPACE_SUBDIRS`）。
///
/// `worker-home` 放外部 worker runtime 的运行时状态（外部 CLI 的
/// `CODEX_HOME` 之类），**故意不在 `artifacts/` 里**：worker 的 cwd 就是
/// `artifacts/`，把它的状态写进去等于让 worker 一找工作目录就遍历到几千个
/// 自己的文件，步预算全耗在"我是谁我在哪"上。工件本身仍落
/// `artifacts/worker/<run>/artifacts/`，随 mission 一起归档取证。
pub const MISSION_WORKSPACE_SUBDIRS: [&str; 10] = [
    "uploads",
    "scripts",
    "artifacts",
    "evidence",
    "findings",
    "reports",
    "scratch",
    "logs",
    "exports",
    "worker-home",
];

/// Mission workspace 失败（Python ValueError / HTTPException 422/500 对应）。
#[derive(Debug, thiserror::Error)]
pub enum MissionWorkspaceError {
    /// Path escapes the configured workspace root.
    #[error("path escapes mission workspace root: {0}")]
    PathEscape(PathBuf),
    /// Underlying filesystem failure.
    #[error("mission workspace io failed: {0}")]
    Io(#[from] std::io::Error),
    /// Every candidate upload name 1..10_000 was taken.
    #[error("could not allocate mission upload filename")]
    AllocationExhausted,
}

/// Resolve the Mission workspace root from the environment once, mirroring
/// Python's per-process `LYNCEUS_MISSION_WORKSPACE_DIR` / `LYNCEUS_WORKSPACE_DIR`
/// lookup. Callers pass the returned root explicitly so tests can inject
/// isolated directories.
#[must_use]
pub fn mission_workspace_root_from_env() -> PathBuf {
    if let Some(raw) = std::env::var_os("LYNCEUS_MISSION_WORKSPACE_DIR")
        && !raw.is_empty()
    {
        return resolved_path(Path::new(&raw));
    }
    if let Some(workspace) = std::env::var_os("LYNCEUS_WORKSPACE_DIR")
        && !workspace.is_empty()
    {
        return resolved_path(&PathBuf::from(workspace).join("missions"));
    }
    resolved_path(Path::new("data/missions"))
}

/// Return the configured Mission workspace root, creating it if needed.
///
/// # Errors
/// Root creation fails.
pub fn mission_workspace_root(root: &Path) -> Result<PathBuf, MissionWorkspaceError> {
    let resolved = resolved_path(root);
    std::fs::create_dir_all(&resolved)?;
    Ok(resolved)
}

/// Return a filesystem-safe ASCII slug derived from user-visible mission text.
#[must_use]
pub fn safe_mission_slug(text: &str, max_length: usize) -> String {
    let ascii_text: String = text.nfkd().filter(char::is_ascii).collect();
    let mut slug: String = ascii_text
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                character
            } else {
                '-'
            }
        })
        .collect();
    slug = slug.trim_matches('-').to_string();
    if slug.len() > max_length {
        slug = slug[..max_length].trim_matches('-').to_string();
    }
    if slug.is_empty() {
        "mission".to_string()
    } else {
        slug
    }
}

/// Build the canonical workspace path under `root` without creating it.
///
/// # Errors
/// The resolved path escapes the root.
pub fn build_mission_workspace_path(
    root: &Path,
    mission_id: &str,
    user_goal: &str,
    created_at: &Timestamp,
) -> Result<PathBuf, MissionWorkspaceError> {
    let root_resolved = resolved_path(root);
    let timestamp = created_at.workspace_stamp();
    let name = format!(
        "{}_{}_{}",
        timestamp,
        safe_mission_slug(user_goal, 48),
        mission_id_short(mission_id)
    );
    let path = resolved_path(&root_resolved.join(name));
    require_relative_to(&path, &root_resolved)?;
    Ok(path)
}

/// Create and return the canonical workspace directory for a Mission id.
///
/// # Errors
/// Root/path creation fails or the resolved path escapes the root.
pub fn ensure_mission_workspace(
    root: &Path,
    mission_id: &str,
    user_goal: &str,
    created_at: &Timestamp,
) -> Result<PathBuf, MissionWorkspaceError> {
    let root = mission_workspace_root(root)?;
    let path = build_mission_workspace_path(&root, mission_id, user_goal, created_at)?;
    std::fs::create_dir_all(&path)?;
    for subdir in MISSION_WORKSPACE_SUBDIRS {
        std::fs::create_dir_all(path.join(subdir))?;
    }
    Ok(path)
}

/// Return the Mission uploads directory, creating the workspace if needed.
///
/// # Errors
/// Workspace path escapes the configured root or creation fails.
pub fn mission_uploads_dir(
    root: &Path,
    mission: &Mission,
) -> Result<PathBuf, MissionWorkspaceError> {
    let workspace = workspace_path_from_mission(root, mission)?;
    let uploads = workspace.join("uploads");
    let uploads = require_relative_to(&uploads, &workspace)?;
    std::fs::create_dir_all(&uploads)?;
    Ok(uploads)
}

/// Return an existing or newly-created workspace path for `mission`.
///
/// # Errors
/// The stored workspace path escapes the configured root or creation fails.
pub fn workspace_path_from_mission(
    root: &Path,
    mission: &Mission,
) -> Result<PathBuf, MissionWorkspaceError> {
    let raw = mission
        .metadata
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ensure_mission_workspace(
            root,
            mission.id.as_str(),
            &mission.user_goal,
            &mission.created_at,
        );
    };
    let workspace = resolved_path(Path::new(raw));
    let root = mission_workspace_root(root)?;
    require_relative_to(&workspace, &root)?;
    std::fs::create_dir_all(&workspace)?;
    for subdir in MISSION_WORKSPACE_SUBDIRS {
        std::fs::create_dir_all(workspace.join(subdir))?;
    }
    Ok(workspace)
}

/// Return metadata fields stored on Mission and Artifact records.
#[must_use]
pub fn mission_workspace_metadata(workspace_path: &Path, mission_id: &str) -> Map<String, Value> {
    let resolved = resolved_path(workspace_path);
    Map::from_iter([
        (
            "mission_id".to_string(),
            Value::String(mission_id.to_string()),
        ),
        (
            "workspace_path".to_string(),
            Value::String(resolved.to_string_lossy().into_owned()),
        ),
        (
            "workspace_name".to_string(),
            Value::String(resolved.file_name().map_or_else(
                || resolved.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            )),
        ),
    ])
}

/// Return `mission` with workspace metadata merged in.
#[must_use]
pub fn mission_with_workspace_metadata(mission: &Mission, workspace_path: &Path) -> Mission {
    let mut metadata = mission.metadata.clone();
    metadata.extend(mission_workspace_metadata(
        workspace_path,
        mission.id.as_str(),
    ));
    let mut updated = mission.clone();
    updated.metadata = metadata;
    updated
}

/// Return a collision-safe path under the Mission uploads directory.
///
/// # Errors
/// Workspace path escapes the configured root or allocation fails.
pub fn allocate_input_path(
    root: &Path,
    mission: &Mission,
    filename: Option<&str>,
) -> Result<PathBuf, MissionWorkspaceError> {
    let uploads = mission_uploads_dir(root, mission)?;
    let safe_name = safe_upload_filename(filename);
    let candidate = require_relative_to(&resolved_path(&uploads.join(&safe_name)), &uploads)?;
    if !candidate.exists() {
        return Ok(candidate);
    }

    let stem = Path::new(&safe_name).file_stem().map_or_else(
        || "upload".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let suffix = Path::new(&safe_name)
        .extension()
        .map_or_else(String::new, |extension| {
            format!(".{}", extension.to_string_lossy())
        });
    for index in 1..10_000 {
        let candidate = require_relative_to(
            &resolved_path(&uploads.join(format!("{stem}-{index}{suffix}"))),
            &uploads,
        )?;
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(MissionWorkspaceError::AllocationExhausted)
}

/// Return `artifact` updated to point at a Mission workspace path.
///
/// # Errors
/// The path escapes the Mission workspace.
pub fn artifact_with_workspace_metadata(
    root: &Path,
    artifact: &ArtifactRecord,
    mission: &Mission,
    path: &Path,
) -> Result<ArtifactRecord, MissionWorkspaceError> {
    let workspace = workspace_path_from_mission(root, mission)?;
    let resolved = resolved_path(path);
    require_relative_to(&resolved, &workspace)?;
    let relative = relative_posix(&resolved, &workspace)?;
    let mut metadata = artifact.metadata.clone();
    metadata.insert(
        "mission_id".to_string(),
        Value::String(mission.id.as_str().to_string()),
    );
    // Artifact-level workspace keys differ from Mission-level ones
    // (`mission_workspace_*` here, `workspace_*` on the Mission).
    metadata.insert(
        "mission_workspace_path".to_string(),
        Value::String(workspace.to_string_lossy().into_owned()),
    );
    metadata.insert(
        "mission_workspace_name".to_string(),
        Value::String(workspace.file_name().map_or_else(
            || workspace.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        )),
    );
    metadata.insert(
        "workspace_relative_path".to_string(),
        Value::String(relative),
    );
    metadata.insert(
        "stored_filename".to_string(),
        Value::String(
            resolved
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        ),
    );
    let mut updated = artifact.clone();
    updated.project_id = Some(mission.project_id.clone());
    updated.uri = resolved.to_string_lossy().into_owned();
    updated.metadata = metadata;
    Ok(updated)
}

fn mission_id_short(mission_id: &str) -> String {
    let cleaned: String = mission_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect();
    let cleaned = cleaned.trim_matches(['_', '-']);
    if cleaned.is_empty() {
        return "mission".to_string();
    }
    if let Some((prefix, suffix)) = cleaned.split_once('_') {
        let prefix = &prefix[..prefix.len().min(3)];
        let prefix = if prefix.is_empty() { "mis" } else { prefix };
        let suffix = &suffix[..suffix.len().min(8)];
        return if suffix.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix}_{suffix}")
        };
    }
    cleaned[..cleaned.len().min(12)].to_string()
}

/// Resolve with Python's non-strict `Path.resolve()` semantics and strip the
/// Windows verbatim prefix so recorded paths keep the plain form.
pub(crate) fn resolved_path(path: &Path) -> PathBuf {
    let normalized = storage::journal::normalize(path);
    let text = normalized.as_os_str().to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{stripped}"));
    }
    if let Some(stripped) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(stripped);
    }
    normalized
}

fn require_relative_to(path: &Path, root: &Path) -> Result<PathBuf, MissionWorkspaceError> {
    if path.starts_with(root) {
        Ok(path.to_path_buf())
    } else {
        Err(MissionWorkspaceError::PathEscape(path.to_path_buf()))
    }
}

fn relative_posix(path: &Path, root: &Path) -> Result<String, MissionWorkspaceError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| MissionWorkspaceError::PathEscape(path.to_path_buf()))?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

/// 各域配置段需要注入 `artifact_dir` 的工具名
/// （Python `MISSION_TOOL_CONFIG_SECTIONS`；空串项仅为对齐 Python 元组长度）。
pub const MISSION_TOOL_CONFIG_SECTIONS: [(&str, [&str; 3]); 11] = [
    ("web_sast", ["semgrep", "", ""]),
    ("web_dast", ["nuclei", "", ""]),
    ("web_recon", ["katana", "jsluice", ""]),
    ("content_discovery", ["ffuf", "feroxbuster", "gobuster"]),
    (
        "fingerprint_intelligence",
        ["native_fingerprint", "wappalyzergo", "ehole"],
    ),
    ("asset_recon", ["subfinder", "naabu", "httpx"]),
    ("code_deep_sast", ["semgrep", "", ""]),
    ("web_validation", ["dalfox", "", ""]),
    ("exploitability_validation", ["afrog", "", ""]),
    ("internal_surface", ["", "", ""]),
    ("traffic_intelligence", ["traffic_import", "", ""]),
];

/// 把 Mission 本地 artifact/log/scratch 目录注入 run 配置
/// （Python `config_with_mission_workspace`）。
///
/// # Errors
/// workspace 路径越界或创建失败。
pub fn config_with_mission_workspace(
    mut config: serde_json::Map<String, serde_json::Value>,
    root: &Path,
    mission: &Mission,
) -> Result<serde_json::Map<String, serde_json::Value>, MissionWorkspaceError> {
    use serde_json::Value;

    let workspace = workspace_path_from_mission(root, mission)?;
    let artifacts_dir = mission_subdir(&workspace, "artifacts")?;
    let logs_dir = mission_subdir(&workspace, "logs")?;
    let scratch_dir = mission_subdir(&workspace, "scratch")?;
    config.insert(
        "mission_workspace_path".to_string(),
        Value::String(workspace.to_string_lossy().into_owned()),
    );
    config.insert(
        "artifact_dir".to_string(),
        Value::String(artifacts_dir.to_string_lossy().into_owned()),
    );
    config.insert(
        "log_dir".to_string(),
        Value::String(logs_dir.to_string_lossy().into_owned()),
    );
    config.insert(
        "scratch_dir".to_string(),
        Value::String(scratch_dir.to_string_lossy().into_owned()),
    );
    for (section_name, tool_names) in MISSION_TOOL_CONFIG_SECTIONS {
        let Some(Value::Object(section)) = config.get(section_name) else {
            continue;
        };
        let mut section = section.clone();
        section
            .entry("artifact_dir".to_string())
            .or_insert_with(|| Value::String(artifacts_dir.to_string_lossy().into_owned()));
        for tool_name in tool_names {
            if tool_name.is_empty() {
                continue;
            }
            if let Some(Value::Object(tool_cfg)) = section.get(tool_name) {
                let mut tool_cfg = tool_cfg.clone();
                tool_cfg
                    .entry("artifact_dir".to_string())
                    .or_insert_with(|| Value::String(artifacts_dir.to_string_lossy().into_owned()));
                section.insert(tool_name.to_string(), Value::Object(tool_cfg));
            }
        }
        config.insert(section_name.to_string(), Value::Object(section));
    }
    Ok(config)
}

/// 返回 Mission workspace 的一个标准子目录（Python `mission_subdir`）。
///
/// # Errors
/// 子目录名不在标准清单、路径越界或创建失败。
pub fn mission_subdir(workspace: &Path, name: &str) -> Result<PathBuf, MissionWorkspaceError> {
    if !MISSION_WORKSPACE_SUBDIRS.contains(&name) {
        return Err(MissionWorkspaceError::PathEscape(PathBuf::from(format!(
            "unsupported mission workspace subdir: {name}"
        ))));
    }
    let subdir = require_relative_to(&workspace.join(name), workspace)?;
    std::fs::create_dir_all(&subdir)?;
    Ok(subdir)
}

/// 把既有本地工件移入 Mission 的 `uploads/` 目录
/// （Python `move_artifact_into_mission_uploads`）。
///
/// # Errors
/// 源文件缺失、路径越界或移动失败。
pub fn move_artifact_into_mission_uploads(
    root: &Path,
    artifact: &ArtifactRecord,
    mission: &Mission,
) -> Result<ArtifactRecord, MissionWorkspaceError> {
    use serde_json::Value;

    let source = resolved_path(Path::new(&artifact.uri));
    if !source.is_file() {
        return Err(MissionWorkspaceError::PathEscape(PathBuf::from(format!(
            "artifact file not found: {}",
            artifact.uri
        ))));
    }
    let uploads = mission_uploads_dir(root, mission)?;
    if source.starts_with(&uploads) {
        return artifact_with_workspace_metadata(root, artifact, mission, &source);
    }
    let original_filename = artifact
        .metadata
        .get("original_filename")
        .and_then(Value::as_str);
    let stored_filename = artifact
        .metadata
        .get("stored_filename")
        .and_then(Value::as_str);
    let filename = original_filename
        .filter(|value| !value.trim().is_empty())
        .or(stored_filename.filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| {
            source
                .file_name()
                .map_or("upload.bin", |name| name.to_str().unwrap_or("upload.bin"))
        });
    let destination = allocate_input_path(root, mission, Some(filename))?;
    if source == destination {
        return artifact_with_workspace_metadata(root, artifact, mission, &destination);
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(&source, &destination).is_err() {
        std::fs::copy(&source, &destination).map_err(|error| {
            MissionWorkspaceError::PathEscape(PathBuf::from(format!(
                "failed to move artifact: {error}"
            )))
        })?;
        std::fs::remove_file(&source).map_err(|error| {
            MissionWorkspaceError::PathEscape(PathBuf::from(format!(
                "failed to move artifact: {error}"
            )))
        })?;
    }
    artifact_with_workspace_metadata_ex(root, artifact, mission, &destination, Some(&source))
}

/// [`artifact_with_workspace_metadata`] 的全参形式（previous_uri 记录移动前位置）。
///
/// # Errors
/// 路径越出 Mission workspace。
pub fn artifact_with_workspace_metadata_ex(
    root: &Path,
    artifact: &ArtifactRecord,
    mission: &Mission,
    path: &Path,
    previous_uri: Option<&Path>,
) -> Result<ArtifactRecord, MissionWorkspaceError> {
    let workspace = workspace_path_from_mission(root, mission)?;
    let resolved = resolved_path(path);
    require_relative_to(&resolved, &workspace)?;
    let relative = relative_posix(&resolved, &workspace)?;
    let mut metadata = artifact.metadata.clone();
    if let Some(previous) = previous_uri {
        metadata
            .entry("original_uri".to_string())
            .or_insert_with(|| serde_json::Value::String(artifact.uri.clone()));
        metadata.insert(
            "previous_uri".to_string(),
            serde_json::Value::String(previous.to_string_lossy().into_owned()),
        );
    }
    metadata.extend(mission_workspace_metadata(&workspace, mission.id.as_str()));
    metadata.insert(
        "workspace_relative_path".to_string(),
        serde_json::Value::String(relative),
    );
    metadata.insert(
        "stored_filename".to_string(),
        serde_json::Value::String(
            resolved
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        ),
    );
    let mut updated = artifact.clone();
    updated.project_id = Some(mission.project_id.clone());
    updated.uri = resolved.to_string_lossy().into_owned();
    updated.metadata = metadata;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::Timestamp;

    fn fixed_timestamp() -> Timestamp {
        serde_json::from_str::<Timestamp>("\"2026-06-23T15:30:12Z\"")
            .expect("fixed timestamp must parse")
    }

    #[test]
    fn safe_mission_slug_normalizes_user_goal() {
        assert_eq!(
            safe_mission_slug("../../Firmware Analysis!!", 48),
            "firmware-analysis"
        );
        assert_eq!(safe_mission_slug("   ", 48), "mission");
        assert_eq!(safe_mission_slug(&"x".repeat(100), 12).len(), 12);
        // Non-ASCII goals degrade to the default slug like the Python
        // NFKD + ascii-ignore pipeline.
        assert_eq!(safe_mission_slug("固件分析", 48), "mission");
    }

    #[test]
    fn build_mission_workspace_path_stays_under_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace").join("missions");
        let path = build_mission_workspace_path(
            &root,
            "../../../mis_escape",
            "../../Firmware",
            &fixed_timestamp(),
        )
        .expect("path must build");

        let resolved_root = resolved_path(&root);
        assert!(path.starts_with(&resolved_root));
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("20260623-153012_firmware_"), "{name}");
        assert!(!name.contains(".."));
    }

    #[test]
    fn build_mission_workspace_path_sanitizes_escaped_parts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace").join("missions");
        let path = build_mission_workspace_path(
            &root,
            "../../../../etc",
            "../../../../goal",
            &fixed_timestamp(),
        )
        .expect("path must build");
        assert!(path.starts_with(resolved_path(&root)));
    }

    #[test]
    fn ensure_mission_workspace_creates_standard_subdirs() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace").join("missions");
        let workspace = ensure_mission_workspace(
            &root,
            "mission_abcdef123456",
            "Firmware analysis",
            &fixed_timestamp(),
        )
        .expect("workspace must create");

        let name = workspace
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "20260623-153012_firmware-analysis_mis_abcdef12");
        for subdir in MISSION_WORKSPACE_SUBDIRS {
            assert!(workspace.join(subdir).is_dir(), "missing {subdir}");
        }
        // Second call reuses the stored path without recreating anything.
        let again = ensure_mission_workspace(
            &root,
            "mission_abcdef123456",
            "Firmware analysis",
            &fixed_timestamp(),
        )
        .expect("workspace must be stable");
        assert_eq!(again, workspace);
    }

    #[test]
    fn allocate_input_path_avoids_collisions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace").join("missions");
        let mut mission = models::Mission::new(
            models::ProjectId::new("proj".to_string()),
            "upload goal".to_string(),
        );
        mission.id = models::MissionId::new("mission_abcdef123456".to_string());

        let first = allocate_input_path(&root, &mission, Some("report.log")).expect("allocate");
        std::fs::create_dir_all(first.parent().expect("parent")).expect("mkdir");
        std::fs::write(&first, b"payload").expect("seed file");
        let second = allocate_input_path(&root, &mission, Some("report.log")).expect("allocate");
        assert_ne!(first, second);
        let name = second
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, "report-1.log");
    }

    #[test]
    fn workspace_path_from_metadata_must_stay_under_root() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace").join("missions");
        let mut mission = models::Mission::new(
            models::ProjectId::new("proj".to_string()),
            "goal".to_string(),
        );
        mission.id = models::MissionId::new("mission".to_string());
        mission.metadata.insert(
            "workspace_path".to_string(),
            serde_json::json!(directory.path().join("outside")),
        );

        let result = workspace_path_from_mission(&root, &mission);
        assert!(
            result.is_err(),
            "workspace outside the configured root must be rejected"
        );
    }
}
