//! Upload intake helpers shared by the upload route and Mission intake.
//!
//! Mirrors `server/api/upload_intake.py`: deterministic filename
//! sanitization, extension/magic-byte classification, artifact summaries and
//! `MissionAsset` construction. Classification never inspects file contents
//! beyond the first 16 magic bytes supplied by the caller.

use std::collections::HashSet;
use std::path::Path;

use models::{
    ArtifactKind, ArtifactRecord, AuditDomain, Mission, MissionAsset, MissionAssetId,
    MissionAssetSensitivity, MissionAssetSource, MissionAssetType, MissionId, ProjectId,
};
use serde_json::{Map, Value};

/// Lightweight classification result for one uploaded artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadClassification {
    /// Normalized extension (compound archive suffixes preserved).
    pub extension: String,
    /// Client-provided or inferred MIME type.
    pub mime_type: Option<String>,
    /// Deterministic input kind, e.g. `source_archive`.
    pub detected_input_type: &'static str,
    /// Deterministic target kind, e.g. `binary`.
    pub detected_target_type: &'static str,
    /// Artifact record kind.
    pub artifact_kind: ArtifactKind,
}

/// Return a single safe basename suitable for storing under the upload root.
#[must_use]
pub fn safe_upload_filename(filename: Option<&str>) -> String {
    let raw = filename.unwrap_or("upload.bin").replace('\\', "/");
    let raw = raw.rsplit('/').next().unwrap_or_default().trim();
    let raw = if raw.is_empty() || raw == "." || raw == ".." {
        "upload.bin"
    } else {
        raw
    };
    let safe: String = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = safe.trim_matches(|c| c == '.' || c == '_');
    if trimmed.is_empty() {
        "upload.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Return a normalized extension, preserving common archive compound suffixes.
#[must_use]
pub fn upload_extension(filename: &str) -> String {
    let lowered = filename.to_lowercase();
    for suffix in [".tar.gz", ".tar.bz2", ".tar.xz"] {
        if lowered.ends_with(suffix) {
            return suffix.to_string();
        }
    }
    Path::new(&lowered)
        .extension()
        .map_or_else(String::new, |extension| {
            format!(".{}", extension.to_string_lossy())
        })
}

const SOURCE_ARCHIVE_EXTENSIONS: [&str; 6] = [".zip", ".tar", ".tar.gz", ".tgz", ".7z", ".rar"];
const BINARY_EXTENSIONS: [&str; 8] = [
    ".exe", ".dll", ".so", ".dylib", ".bin", ".elf", ".apk", ".ipa",
];
const FIRMWARE_IMAGE_EXTENSIONS: [&str; 18] = [
    ".fw",
    ".firmware",
    ".img",
    ".rom",
    ".ubi",
    ".ubifs",
    ".squashfs",
    ".cramfs",
    ".jffs2",
    ".ext4",
    ".iso",
    ".vmdk",
    ".qcow2",
    ".raw",
    ".dd",
    ".dump",
    ".mem",
    ".dmp",
];
const TRAFFIC_EXTENSIONS: [&str; 4] = [".pcap", ".pcapng", ".har", ".saz"];
const DATASET_EXTENSIONS: [&str; 4] = [".xlsx", ".xls", ".csv", ".tsv"];
const EVIDENCE_EXTENSIONS: [&str; 7] =
    [".log", ".txt", ".json", ".ndjson", ".xml", ".yaml", ".yml"];

/// Classify an upload using filename, content type, and a small magic-byte sample.
#[must_use]
pub fn classify_upload(
    filename: &str,
    mime_type: Option<&str>,
    magic_bytes: &[u8],
) -> UploadClassification {
    let extension = upload_extension(filename);
    let detected = classification_from_extension(&extension)
        .or_else(|| classification_from_magic(magic_bytes))
        .unwrap_or(("unknown_artifact", "unknown", ArtifactKind::Other));

    let (detected_input_type, detected_target_type, artifact_kind) = detected;
    let resolved_mime = mime_type
        .map(str::to_string)
        .or_else(|| guess_mime_type(filename, &extension));
    UploadClassification {
        extension,
        mime_type: resolved_mime,
        detected_input_type,
        detected_target_type,
        artifact_kind,
    }
}

fn classification_from_extension(
    extension: &str,
) -> Option<(&'static str, &'static str, ArtifactKind)> {
    if SOURCE_ARCHIVE_EXTENSIONS.contains(&extension) {
        return Some(("source_archive", "source", ArtifactKind::Other));
    }
    if BINARY_EXTENSIONS.contains(&extension) {
        return Some(("binary", "binary", ArtifactKind::Other));
    }
    if FIRMWARE_IMAGE_EXTENSIONS.contains(&extension) {
        return Some(("firmware_image", "binary", ArtifactKind::Other));
    }
    if TRAFFIC_EXTENSIONS.contains(&extension) {
        let kind = if extension == ".har" {
            ArtifactKind::Har
        } else {
            ArtifactKind::Trace
        };
        return Some(("traffic_capture", "traffic", kind));
    }
    if DATASET_EXTENSIONS.contains(&extension) {
        return Some(("investigation_dataset", "mixed", ArtifactKind::Other));
    }
    if EVIDENCE_EXTENSIONS.contains(&extension) {
        let kind = if extension == ".log" {
            ArtifactKind::Log
        } else {
            ArtifactKind::RawOutput
        };
        return Some(("evidence_document", "mixed", kind));
    }
    if extension == ".sarif" {
        return Some(("scanner_result", "source", ArtifactKind::Sarif));
    }
    None
}

fn classification_from_magic(
    magic_bytes: &[u8],
) -> Option<(&'static str, &'static str, ArtifactKind)> {
    let sample = &magic_bytes[..magic_bytes.len().min(16)];
    if sample.starts_with(b"MZ") || sample.starts_with(b"\x7fELF") {
        return Some(("binary", "binary", ArtifactKind::Other));
    }
    let pcap_magics: [&[u8]; 5] = [
        b"\xd4\xc3\xb2\xa1",
        b"\xa1\xb2\xc3\xd4",
        b"\x4d\x3c\xb2\xa1",
        b"\xa1\xb2\x3c\x4d",
        b"\x0a\x0d\x0d\x0a",
    ];
    if pcap_magics.iter().any(|magic| sample.starts_with(magic)) {
        return Some(("traffic_capture", "traffic", ArtifactKind::Trace));
    }
    if sample.starts_with(b"PK\x03\x04") {
        return Some(("source_archive", "source", ArtifactKind::Other));
    }
    None
}

/// Mirror `CPython` `mimetypes.guess_type` for the upload-relevant suffixes.
///
/// The table reflects the values observed under `CPython` 3.13 on Windows,
/// where `mimetypes` augments its built-in map with registry entries
/// (e.g. `.csv` -> `application/vnd.ms-excel`); this is the actual dual-run
/// behavior rather than the idealized cross-platform table.
fn guess_mime_type(filename: &str, extension: &str) -> Option<String> {
    if extension == ".sarif" {
        return Some("application/sarif+json".to_string());
    }
    let lowered = filename.to_lowercase();
    MIMETYPES_BY_SUFFIX
        .iter()
        .find(|(suffix, _)| lowered.ends_with(suffix))
        .map(|(_, mime)| (*mime).to_string())
}

/// `(suffix, wire value)` pairs; `None` entries from `CPython` are omitted.
const MIMETYPES_BY_SUFFIX: [(&str, &str); 19] = [
    (".zip", "application/x-zip-compressed"),
    (".tar", "application/x-tar"),
    (".tar.gz", "application/x-tar"),
    (".tar.bz2", "application/x-tar"),
    (".tar.xz", "application/x-tar"),
    (".tgz", "application/x-tar"),
    (".7z", "application/x-compressed"),
    (".rar", "application/x-compressed"),
    (".exe", "application/x-msdownload"),
    (".dll", "application/x-msdownload"),
    (".so", "application/octet-stream"),
    (".bin", "application/octet-stream"),
    (".txt", "text/plain"),
    (".json", "application/json"),
    (".csv", "application/vnd.ms-excel"),
    (".tsv", "text/tab-separated-values"),
    (".xls", "application/vnd.ms-excel"),
    (
        ".xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (".xml", "text/xml"),
];

/// Return metadata safe to embed in Mission target and intake plans.
///
/// Key order mirrors the Python dict insertion order.
#[must_use]
pub fn artifact_summary(artifact: &ArtifactRecord) -> Map<String, Value> {
    let metadata = &artifact.metadata;
    let mut summary = Map::new();
    summary.insert(
        "artifact_record_id".to_string(),
        Value::String(artifact.id.as_str().to_string()),
    );
    summary.insert(
        "kind".to_string(),
        Value::String(artifact.kind.as_str().to_string()),
    );
    summary.insert("uri".to_string(), Value::String(artifact.uri.clone()));
    summary.insert(
        "storage_backend".to_string(),
        Value::String(artifact.storage_backend.clone()),
    );
    for key in [
        "original_filename",
        "stored_filename",
        "extension",
        "detected_input_type",
        "detected_target_type",
        "mission_id",
        "mission_workspace_path",
        "mission_workspace_name",
        "workspace_relative_path",
    ] {
        let value = string_metadata(metadata, key);
        summary.insert(key.to_string(), Value::String(value));
    }
    summary.insert(
        "mime_type".to_string(),
        artifact
            .mime_type
            .clone()
            .map_or(Value::Null, Value::String),
    );
    summary.insert(
        "size_bytes".to_string(),
        artifact
            .size_bytes
            .map_or(Value::Null, |size| Value::Number(size.into())),
    );
    summary.insert(
        "sha256".to_string(),
        artifact.sha256.clone().map_or(Value::Null, Value::String),
    );
    summary
}

/// Return stable summaries for embedding in API responses and mission metadata.
#[must_use]
pub fn artifact_summaries(artifacts: &[ArtifactRecord]) -> Vec<Map<String, Value>> {
    artifacts.iter().map(artifact_summary).collect()
}

/// Infer the artifact-derived target type string from artifact metadata.
///
/// 返回 `"source"` / `"binary"` / `"traffic"` / `"mixed"` / `"unknown"`。
/// 上传物给出的是真实信号（用户交的是 pcap 还是源码树），不是对模糊
/// 输入的猜测——目标分类枚举已删，这里只保留可审计的字符串。
#[must_use]
pub fn artifact_target_type(artifacts: &[ArtifactRecord]) -> &'static str {
    let target_types: HashSet<String> = artifacts
        .iter()
        .map(|artifact| {
            artifact
                .metadata
                .get("detected_target_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_lowercase()
        })
        .filter(|value| !value.is_empty() && value != "unknown")
        .collect();
    if target_types.len() == 1 {
        for known in ["source", "binary", "traffic"] {
            if target_types.contains(known) {
                return known;
            }
        }
    }
    if target_types.is_empty() {
        "unknown"
    } else {
        "mixed"
    }
}

/// Select the closest audit domain for an artifact-derived target type.
#[must_use]
pub fn audit_domain_for_target_type(target_type: &str) -> AuditDomain {
    match target_type {
        "source" => AuditDomain::WebSast,
        "binary" => AuditDomain::BinaryStatic,
        "traffic" => AuditDomain::TrafficIntelligence,
        "url" => AuditDomain::WebDast,
        "cloud" => AuditDomain::CloudNative,
        _ => AuditDomain::Composite,
    }
}

/// Build string-only Mission target fields for artifact references.
///
/// `artifacts_summary` is serialized with Python `json.dumps` default
/// separators (`", "` / `": "`) and `ensure_ascii=False`, insertion order.
#[must_use]
pub fn mission_target_updates(artifacts: &[ArtifactRecord]) -> Map<String, Value> {
    if artifacts.is_empty() {
        return Map::new();
    }
    let mut updates = Map::new();
    updates.insert(
        "artifact_record_ids".to_string(),
        Value::String(
            artifacts
                .iter()
                .map(|artifact| artifact.id.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ),
    );
    updates.insert(
        "artifacts_summary".to_string(),
        Value::String(python_json_dumps(&Value::Array(
            artifact_summaries(artifacts)
                .into_iter()
                .map(Value::Object)
                .collect(),
        ))),
    );
    if let [artifact] = artifacts {
        let detected_input_type = string_metadata(&artifact.metadata, "detected_input_type");
        match detected_input_type.as_str() {
            "source_archive" => {
                updates.insert("repo_path".to_string(), Value::String(artifact.uri.clone()));
                updates.insert(
                    "source_archive".to_string(),
                    Value::String(artifact.uri.clone()),
                );
            }
            "binary" | "firmware_image" => {
                updates.insert("binary".to_string(), Value::String(artifact.uri.clone()));
            }
            "traffic_capture" => {
                updates.insert(
                    "artifact_path".to_string(),
                    Value::String(artifact.uri.clone()),
                );
                updates.insert(
                    "traffic_artifact".to_string(),
                    Value::String(artifact.uri.clone()),
                );
            }
            _ => {
                updates.insert(
                    "artifact_path".to_string(),
                    Value::String(artifact.uri.clone()),
                );
            }
        }
    }
    updates
}

/// Serialize JSON the way Python `json.dumps(..., ensure_ascii=False)` does
/// with default separators (`, ` / `: `), preserving insertion order.
#[must_use]
pub fn python_json_dumps(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => python_json_string(text),
        Value::Array(items) => {
            let body = items
                .iter()
                .map(python_json_dumps)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{body}]")
        }
        Value::Object(entries) => {
            let body = entries
                .iter()
                .map(|(key, value)| {
                    format!("{}: {}", python_json_string(key), python_json_dumps(value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{body}}}")
        }
    }
}

fn python_json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Create or merge the `MissionAsset` representation for an uploaded artifact.
///
/// Persistence goes through [`crate::solvers`]'s manager facade; this
/// constructor mirrors Python `bind_artifact_to_mission` field by field.
///
/// # Errors
/// Asset value validation (blank value) fails.
pub fn bind_artifact_to_mission(
    artifact: &ArtifactRecord,
    mission: &Mission,
    source: MissionAssetSource,
) -> Result<MissionAsset, String> {
    let mut metadata = artifact.metadata.clone();
    let detected_input_type = string_metadata(&artifact.metadata, "detected_input_type");
    let detected_target_type = string_metadata(&artifact.metadata, "detected_target_type");
    metadata.insert(
        "artifact_record_id".to_string(),
        Value::String(artifact.id.as_str().to_string()),
    );
    metadata.insert(
        "artifact_uri".to_string(),
        Value::String(artifact.uri.clone()),
    );
    metadata.insert(
        "artifact_sha256".to_string(),
        artifact.sha256.clone().map_or(Value::Null, Value::String),
    );
    metadata.insert(
        "storage_backend".to_string(),
        Value::String(artifact.storage_backend.clone()),
    );
    metadata.insert(
        "detected_input_type".to_string(),
        Value::String(detected_input_type.clone()),
    );
    metadata.insert(
        "detected_target_type".to_string(),
        Value::String(detected_target_type.clone()),
    );

    let asset = MissionAsset {
        id: MissionAssetId::new(models::new_id("asset")),
        project_id: ProjectId::new(mission.project_id.as_str().to_string()),
        mission_id: MissionId::new(mission.id.as_str().to_string()),
        asset_type: mission_asset_type_for_artifact(artifact),
        value: artifact_asset_value(artifact),
        label: artifact_asset_label(artifact),
        sensitivity: artifact_asset_sensitivity(artifact),
        confidence: 0.95,
        source,
        source_id: Some(artifact.id.as_str().to_string()),
        branch_id: None,
        run_id: artifact.run_id.clone(),
        evidence_ids: Vec::new(),
        finding_ids: Vec::new(),
        tool_invocation_ids: Vec::new(),
        tags: vec![
            "upload".to_string(),
            "artifact".to_string(),
            detected_input_type,
        ],
        metadata,
        created_at: models::utcnow(),
        updated_at: models::utcnow(),
    };
    asset.validated()
}

/// Map artifact input type onto the Mission asset type.
#[must_use]
pub fn mission_asset_type_for_artifact(artifact: &ArtifactRecord) -> MissionAssetType {
    let detected = string_metadata(&artifact.metadata, "detected_input_type");
    match detected.as_str() {
        "source_archive" => MissionAssetType::SourcePath,
        "binary" | "firmware_image" => MissionAssetType::Binary,
        "traffic_capture" => MissionAssetType::TrafficCapture,
        _ => MissionAssetType::Unknown,
    }
}

/// Stored filename when present, else the artifact URI.
#[must_use]
pub fn artifact_asset_value(artifact: &ArtifactRecord) -> String {
    let stored = string_metadata(&artifact.metadata, "stored_filename");
    if stored.is_empty() {
        artifact.uri.clone()
    } else {
        stored
    }
}

/// Original filename when present.
#[must_use]
pub fn artifact_asset_label(artifact: &ArtifactRecord) -> Option<String> {
    let original = string_metadata(&artifact.metadata, "original_filename");
    if original.is_empty() {
        None
    } else {
        Some(original)
    }
}

/// Sensitivity from artifact metadata, defaulting to unknown.
#[must_use]
pub fn artifact_asset_sensitivity(artifact: &ArtifactRecord) -> MissionAssetSensitivity {
    let raw = artifact
        .metadata
        .get("sensitivity")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    match raw.as_str() {
        "secret" | "credential" | "sensitive" => MissionAssetSensitivity::Sensitive,
        "public" | "non_sensitive" | "non-sensitive" => MissionAssetSensitivity::NonSensitive,
        _ => MissionAssetSensitivity::Unknown,
    }
}

fn string_metadata(metadata: &Map<String, Value>, key: &str) -> String {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map_or_else(String::new, str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ArtifactRecord;
    use serde_json::json;

    #[test]
    fn safe_filename_sanitizes_traversal_and_blanks() {
        assert_eq!(safe_upload_filename(None), "upload.bin");
        assert_eq!(
            safe_upload_filename(Some("..\\..\\secret.json")),
            "secret.json"
        );
        assert_eq!(safe_upload_filename(Some("..")), "upload.bin");
        assert_eq!(safe_upload_filename(Some("   ")), "upload.bin");
        assert_eq!(
            safe_upload_filename(Some("my report (v2).log")),
            "my_report__v2_.log"
        );
        assert_eq!(safe_upload_filename(Some(".hidden")), "hidden");
    }

    #[test]
    fn upload_extension_preserves_archive_compound_suffixes() {
        assert_eq!(upload_extension("bundle.tar.gz"), ".tar.gz");
        assert_eq!(upload_extension("BUNDLE.TAR.BZ2"), ".tar.bz2");
        assert_eq!(upload_extension("capture.pcap"), ".pcap");
        assert_eq!(upload_extension("noext"), "");
    }

    #[test]
    fn classification_table_matches_python_semantics() {
        let case = |filename: &str, magic: &[u8], input: &str, target: &str| {
            let classified = classify_upload(filename, Some("application/octet-stream"), magic);
            assert_eq!(classified.detected_input_type, input, "{filename}");
            assert_eq!(classified.detected_target_type, target, "{filename}");
        };
        case("src.zip", b"PK\x03\x04payload", "source_archive", "source");
        case("sample.exe", b"MZpayload", "binary", "binary");
        case(
            "capture.pcap",
            b"\xd4\xc3\xb2\xa1payload",
            "traffic_capture",
            "traffic",
        );
        case("traffic.har", b"{}", "traffic_capture", "traffic");
        case("cases.csv", b"a,b\n1,2\n", "investigation_dataset", "mixed");
        case(
            "report.xlsx",
            b"PK\x03\x04payload",
            "investigation_dataset",
            "mixed",
        );
        case("router.fw", b"firmware", "firmware_image", "binary");
        case("image.squashfs", b"hsqs", "firmware_image", "binary");
        case("dump.mem", b"\x00\x01mem", "firmware_image", "binary");
        case("unknown.weird", b"payload", "unknown_artifact", "unknown");
        case(
            "evidence.log",
            b"login failed",
            "evidence_document",
            "mixed",
        );
        case("scan.sarif", b"{}", "scanner_result", "source");

        // Extension falls back to magic bytes.
        let by_magic = classify_upload("blob.bin2", None, b"MZpayload");
        assert_eq!(by_magic.detected_input_type, "binary");

        // .har keeps the HAR artifact kind; pcap stays TRACE.
        assert_eq!(
            classify_upload("traffic.har", None, b"{}").artifact_kind,
            ArtifactKind::Har
        );
        assert_eq!(
            classify_upload("capture.pcapng", None, b"").artifact_kind,
            ArtifactKind::Trace
        );
    }

    #[test]
    fn mime_guess_mirrors_cpython_windows_table() {
        assert_eq!(
            classify_upload("cases.csv", None, b"").mime_type.as_deref(),
            Some("application/vnd.ms-excel")
        );
        assert_eq!(
            classify_upload("a.zip", None, b"").mime_type.as_deref(),
            Some("application/x-zip-compressed")
        );
        assert_eq!(
            classify_upload("a.sarif", None, b"").mime_type.as_deref(),
            Some("application/sarif+json")
        );
        assert_eq!(classify_upload("a.har", None, b"").mime_type, None);
        // Client-provided type always wins.
        assert_eq!(
            classify_upload("a.log", Some("text/plain"), b"")
                .mime_type
                .as_deref(),
            Some("text/plain")
        );
    }

    #[test]
    fn artifact_summary_key_order_and_values() {
        let mut metadata = Map::new();
        metadata.insert("original_filename".to_string(), json!("evidence.log"));
        metadata.insert(
            "detected_input_type".to_string(),
            json!("evidence_document"),
        );
        let mut artifact = ArtifactRecord::new("/tmp/evidence.log".to_string());
        artifact.kind = ArtifactKind::Log;
        artifact.storage_backend = "local_filesystem".to_string();
        artifact.mime_type = Some("text/plain".to_string());
        artifact.size_bytes = Some(13);
        artifact.sha256 = Some("abc".to_string());
        artifact.metadata = metadata;

        let summary = artifact_summary(&artifact);
        let keys = summary.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "artifact_record_id",
                "kind",
                "uri",
                "storage_backend",
                "original_filename",
                "stored_filename",
                "extension",
                "detected_input_type",
                "detected_target_type",
                "mission_id",
                "mission_workspace_path",
                "mission_workspace_name",
                "workspace_relative_path",
                "mime_type",
                "size_bytes",
                "sha256",
            ]
        );
        assert_eq!(summary["kind"], json!("log"));
        assert_eq!(summary["size_bytes"], json!(13));
        assert_eq!(summary["original_filename"], json!("evidence.log"));
        assert_eq!(summary["mission_id"], json!(""));
    }

    #[test]
    fn python_json_dumps_matches_default_separators() {
        let mut object = Map::new();
        object.insert("b".to_string(), json!(1));
        object.insert("a".to_string(), json!("中文"));
        let value = Value::Array(vec![Value::Object(object), Value::Bool(false)]);
        assert_eq!(
            python_json_dumps(&value),
            r#"[{"b": 1, "a": "中文"}, false]"#
        );
    }

    #[test]
    fn target_type_inference_prefers_artifacts_only_when_clearer() {
        let artifact = |input: &str, target: &str| {
            let mut record = ArtifactRecord::new(format!("/tmp/{input}"));
            record
                .metadata
                .insert("detected_input_type".to_string(), json!(input));
            record
                .metadata
                .insert("detected_target_type".to_string(), json!(target));
            record
        };
        assert_eq!(artifact_target_type(&[artifact("x", "source")]), "source");
        assert_eq!(
            artifact_target_type(&[artifact("x", "binary"), artifact("y", "traffic")]),
            "mixed"
        );
        // 空 / unknown 元数据不给出任何类型。
        assert_eq!(
            artifact_target_type(&[artifact("x", "unknown"), artifact("y", "")]),
            "unknown"
        );
    }

    #[test]
    fn audit_domain_mapping_matches_python_table() {
        assert_eq!(audit_domain_for_target_type("source"), AuditDomain::WebSast);
        assert_eq!(
            audit_domain_for_target_type("binary"),
            AuditDomain::BinaryStatic
        );
        assert_eq!(
            audit_domain_for_target_type("traffic"),
            AuditDomain::TrafficIntelligence
        );
        assert_eq!(audit_domain_for_target_type("url"), AuditDomain::WebDast);
        assert_eq!(
            audit_domain_for_target_type("cloud"),
            AuditDomain::CloudNative
        );
        assert_eq!(audit_domain_for_target_type("unknown"), AuditDomain::Composite);
    }

    #[test]
    fn bind_artifact_builds_upload_asset_like_python() {
        let mut metadata = Map::new();
        metadata.insert("original_filename".to_string(), json!("capture.pcap"));
        metadata.insert("stored_filename".to_string(), json!("capture.pcap"));
        metadata.insert("detected_input_type".to_string(), json!("traffic_capture"));
        metadata.insert("detected_target_type".to_string(), json!("traffic"));
        metadata.insert("sensitivity".to_string(), json!("public"));
        let mut artifact = ArtifactRecord::new("/ws/uploads/capture.pcap".to_string());
        artifact.sha256 = Some("deadbeef".to_string());
        artifact.storage_backend = "local_filesystem".to_string();
        artifact.metadata = metadata;

        let mut mission = Mission::new(ProjectId::new("proj".to_string()), "goal".to_string());
        mission.id = MissionId::new("mission".to_string());

        let asset = bind_artifact_to_mission(&artifact, &mission, MissionAssetSource::UserTarget)
            .expect("asset must build");
        assert_eq!(asset.asset_type, MissionAssetType::TrafficCapture);
        assert_eq!(asset.value, "capture.pcap");
        assert_eq!(asset.label.as_deref(), Some("capture.pcap"));
        assert_eq!(asset.sensitivity, MissionAssetSensitivity::NonSensitive);
        assert!((asset.confidence - 0.95).abs() < 1e-12);
        assert_eq!(asset.source, MissionAssetSource::UserTarget);
        assert_eq!(asset.source_id.as_deref(), Some(artifact.id.as_str()));
        assert_eq!(asset.tags, ["upload", "artifact", "traffic_capture"]);
        assert_eq!(
            asset.metadata["artifact_record_id"],
            json!(artifact.id.as_str())
        );
        assert_eq!(
            asset.metadata["artifact_uri"],
            json!("/ws/uploads/capture.pcap")
        );
        assert_eq!(asset.metadata["artifact_sha256"], json!("deadbeef"));
        assert_eq!(
            asset.metadata["detected_input_type"],
            json!("traffic_capture")
        );

        // Sensitivity folds secret-ish metadata to sensitive.
        let mut secret_metadata = Map::new();
        secret_metadata.insert("sensitivity".to_string(), json!("SECRET"));
        let mut secret_artifact = ArtifactRecord::new("/tmp/x".to_string());
        secret_artifact.metadata = secret_metadata;
        let secret_asset =
            bind_artifact_to_mission(&secret_artifact, &mission, MissionAssetSource::UserTarget)
                .expect("asset must build");
        assert_eq!(secret_asset.sensitivity, MissionAssetSensitivity::Sensitive);
        assert_eq!(secret_asset.asset_type, MissionAssetType::Unknown);
        assert_eq!(secret_asset.value, "/tmp/x");
        assert!(secret_asset.label.is_none());
    }
}
