//! `/uploads` multipart 路由 —— `server/api/routes/uploads.py` 的 Rust 镜像。
//!
//! 上传流式落盘（分块 SHA-256 + 前 4096 字节魔数采样），引用校验与
//! 工件注册经 [`runtime::AuditManager`] 门面；绝不把大文件内容
//! 嵌进 API 响应或数据库行。

use std::path::{Path as FsPath, PathBuf};

use axum::extract::Multipart;
use axum::extract::multipart::Field;
use axum::http::StatusCode;
use axum::{Json, extract::State};
use engines::upload_intake::classify_upload;
use models::ArtifactRecord;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::ApiError;
use crate::ApiState;
use crate::EngineError;
use crate::mission_workspace;
use crate::mission_workspace::MissionWorkspaceError;

const MAGIC_BYTES_LIMIT: usize = 4096;

/// 上传响应（Python `UploadResponse` wire 形态）。
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    /// 已注册工件。
    pub artifact: ArtifactRecord,
    /// 确定性输入类别。
    pub detected_input_type: String,
    /// 确定性目标类别。
    pub detected_target_type: String,
    /// 内容 SHA-256。
    pub sha256: String,
    /// 字节数。
    pub size_bytes: i64,
}

/// 持久化上传文件并注册 `ArtifactRecord`（Python `/uploads` 路由镜像）。
///
/// # Errors
/// multipart 解析失败（422）、引用不存在（404）/归属不一致（422）、
/// 空文件（422）或落盘/落库失败（500 族）。
pub(crate) async fn upload_artifact(
    State(state): State<ApiState>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let mut purpose = "mission_intake".to_string();
    let mut project_id: Option<String> = None;
    let mut mission_id: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut stored: Option<StoredUpload> = None;
    while let Some(field) = next_field(&mut multipart).await? {
        match field.name() {
            Some("file") => {
                if stored.is_some() {
                    return Err(ApiError(EngineError::Value(
                        "duplicate file field in multipart body".to_string(),
                    )));
                }
                stored = Some(store_upload_stream(field, &state.upload_root).await?);
            }
            Some("purpose") => {
                purpose = read_text(field).await?.unwrap_or_else(|| purpose.clone());
            }
            Some("project_id") => project_id = read_text(field).await?,
            Some("mission_id") => mission_id = read_text(field).await?,
            Some("run_id") => run_id = read_text(field).await?,
            _ => {}
        }
    }
    let upload = stored
        .ok_or_else(|| {
            ApiError(EngineError::Value(
                "multipart body must contain a file field".to_string(),
            ))
        })?
        .into_finalize(purpose, project_id, mission_id, run_id);

    let result = finalize_upload(&state, &upload).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&upload.tmp_path).await;
    }
    result.map(|response| (StatusCode::CREATED, Json(response)))
}

async fn next_field(multipart: &mut Multipart) -> Result<Option<Field<'_>>, ApiError> {
    multipart.next_field().await.map_err(|error| {
        ApiError(EngineError::Value(format!(
            "invalid multipart body: {error}"
        )))
    })
}

async fn read_text(field: Field<'_>) -> Result<Option<String>, ApiError> {
    let text = field.text().await.map_err(|error| {
        ApiError(EngineError::Value(format!(
            "invalid multipart field: {error}"
        )))
    })?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

struct StoredUpload {
    tmp_path: PathBuf,
    upload_root: PathBuf,
    safe_filename: String,
    sha256: String,
    size_bytes: i64,
    magic: Vec<u8>,
    content_type: Option<String>,
    original_filename: Option<String>,
}

struct FinalizeUpload {
    purpose: String,
    project_id: Option<String>,
    mission_id: Option<String>,
    run_id: Option<String>,
    tmp_path: PathBuf,
    upload_root: PathBuf,
    safe_filename: String,
    sha256: String,
    size_bytes: i64,
    magic: Vec<u8>,
    content_type: Option<String>,
    original_filename: Option<String>,
}

impl StoredUpload {
    fn into_finalize(
        self,
        purpose: String,
        project_id: Option<String>,
        mission_id: Option<String>,
        run_id: Option<String>,
    ) -> FinalizeUpload {
        FinalizeUpload {
            purpose,
            project_id,
            mission_id,
            run_id,
            tmp_path: self.tmp_path,
            upload_root: self.upload_root,
            safe_filename: self.safe_filename,
            sha256: self.sha256,
            size_bytes: self.size_bytes,
            magic: self.magic,
            content_type: self.content_type,
            original_filename: self.original_filename,
        }
    }
}

/// 校验引用、分配最终路径、注册工件并绑定 Mission 资产。
async fn finalize_upload(
    state: &ApiState,
    upload: &FinalizeUpload,
) -> Result<UploadResponse, ApiError> {
    let (mission, project_id) = validate_upload_references(state, upload)?;

    let classification = classify_upload(
        &upload.safe_filename,
        upload.content_type.as_deref(),
        &upload.magic,
    );
    let final_path =
        allocate_final_path(&state.mission_workspace_root, upload, mission.as_ref()).await?;
    move_into_place(&upload.tmp_path, &final_path).await?;

    let mut artifact = build_upload_artifact(upload, &classification, &final_path, project_id);
    if let Some(existing) = mission.as_ref() {
        artifact = mission_workspace::artifact_with_workspace_metadata(
            &state.mission_workspace_root,
            &artifact,
            existing,
            &final_path,
        )
        .map_err(|error| map_workspace_error(&error))?;
    }
    let artifact = state.manager.add_artifact_record(&artifact)?;

    if let Some(existing) = mission.as_ref() {
        let asset = engines::upload_intake::bind_artifact_to_mission(
            &artifact,
            existing,
            models::MissionAssetSource::UserTarget,
        )
        .map_err(|error| ApiError(EngineError::Value(error)))?;
        state.manager.upsert_mission_asset(&asset)?;
    }

    Ok(UploadResponse {
        artifact,
        detected_input_type: classification.detected_input_type.to_string(),
        detected_target_type: classification.detected_target_type.to_string(),
        sha256: upload.sha256.clone(),
        size_bytes: upload.size_bytes,
    })
}

/// Mission/project/run 引用校验与空文件拒绝（Python 路由前半段）。
fn validate_upload_references(
    state: &ApiState,
    upload: &FinalizeUpload,
) -> Result<(Option<models::Mission>, Option<String>), ApiError> {
    let mut mission: Option<models::Mission> = None;
    let mut project_id = upload.project_id.clone();
    if let Some(mission_id) = upload.mission_id.as_deref() {
        let found = state
            .manager
            .repository()
            .get_mission(mission_id)?
            .ok_or_else(|| ApiError(EngineError::MissionNotFound(mission_id.to_string())))?;
        if project_id
            .as_deref()
            .is_some_and(|value| value != found.project_id.as_str())
        {
            return Err(ApiError(EngineError::Value(
                "project_id does not match mission project_id".to_string(),
            )));
        }
        project_id = Some(found.project_id.as_str().to_string());
        mission = Some(found);
    }
    if let Some(value) = project_id.as_deref()
        && state.manager.repository().get_project(value)?.is_none()
    {
        return Err(ApiError(EngineError::ProjectNotFound(format!(
            "project not found: {value}"
        ))));
    }
    if let Some(run_id) = upload.run_id.as_deref()
        && state.manager.repository().get_run(run_id)?.is_none()
    {
        return Err(ApiError(EngineError::RunNotFound(format!(
            "run not found: {run_id}"
        ))));
    }
    if upload.size_bytes == 0 {
        return Err(ApiError(EngineError::Value(
            "uploaded file is empty".to_string(),
        )));
    }
    if let Some(existing) = mission.as_mut() {
        let workspace_path =
            mission_workspace::workspace_path_from_mission(&state.mission_workspace_root, existing)
                .map_err(|error| map_workspace_error(&error))?;
        *existing = mission_workspace::mission_with_workspace_metadata(existing, &workspace_path);
        state
            .manager
            .update_mission_record(existing)
            .map_err(ApiError)?;
    }
    Ok((mission, project_id))
}

/// Mission uploads 目录或 staging 去重路径分配，并确保父目录存在。
async fn allocate_final_path(
    root: &FsPath,
    upload: &FinalizeUpload,
    mission: Option<&models::Mission>,
) -> Result<PathBuf, ApiError> {
    let final_path = if let Some(existing) = mission {
        mission_workspace::allocate_input_path(root, existing, Some(&upload.safe_filename))
            .map_err(|error| map_workspace_error(&error))?
    } else {
        collision_safe_path(
            &upload.upload_root,
            &FsPath::new("staging").join(&upload.sha256[..12]),
            &upload.safe_filename,
        )?
    };
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError(EngineError::Value(format!(
                "failed to finalize upload: {error}"
            )))
        })?;
    }
    Ok(final_path)
}

/// 组装待注册的 `ArtifactRecord`（workspace 元数据由调用方按需合并）。
fn build_upload_artifact(
    upload: &FinalizeUpload,
    classification: &engines::upload_intake::UploadClassification,
    final_path: &FsPath,
    project_id: Option<String>,
) -> ArtifactRecord {
    let mut metadata = Map::new();
    metadata.insert(
        "original_filename".to_string(),
        Value::String(upload.original_filename.clone().unwrap_or_default()),
    );
    metadata.insert(
        "stored_filename".to_string(),
        Value::String(file_name(final_path)),
    );
    metadata.insert(
        "extension".to_string(),
        Value::String(classification.extension.clone()),
    );
    metadata.insert(
        "upload_purpose".to_string(),
        Value::String(upload.purpose.clone()),
    );
    metadata.insert(
        "detected_input_type".to_string(),
        Value::String(classification.detected_input_type.to_string()),
    );
    metadata.insert(
        "detected_target_type".to_string(),
        Value::String(classification.detected_target_type.to_string()),
    );
    if let Some(mission_id) = upload.mission_id.as_deref() {
        metadata.insert(
            "mission_id".to_string(),
            Value::String(mission_id.to_string()),
        );
    }

    let mut artifact = ArtifactRecord::new(final_path.to_string_lossy().into_owned());
    artifact.project_id = project_id.map(models::ProjectId::new);
    artifact.run_id = upload.run_id.clone().map(models::RunId::new);
    artifact.kind = classification.artifact_kind;
    artifact.storage_backend = "local_filesystem".to_string();
    artifact.summary = format!(
        "Uploaded {}: {}",
        classification.detected_input_type,
        upload
            .original_filename
            .as_deref()
            .unwrap_or(&upload.safe_filename)
    );
    artifact.mime_type.clone_from(&classification.mime_type);
    artifact.size_bytes = Some(upload.size_bytes);
    artifact.sha256 = Some(upload.sha256.clone());
    artifact.metadata = metadata;
    artifact
}

async fn store_upload_stream(
    mut field: Field<'_>,
    upload_root: &FsPath,
) -> Result<StoredUpload, ApiError> {
    std::fs::create_dir_all(upload_root).map_err(|error| {
        ApiError(EngineError::Value(format!(
            "failed to store upload: {error}"
        )))
    })?;
    let original_filename = field.file_name().map(str::to_string);
    let safe_filename = engines::upload_intake::safe_upload_filename(original_filename.as_deref());
    let tmp_name = format!("{}-{safe_filename}", uuid::Uuid::new_v4().simple());
    let tmp_path = safe_child(upload_root, &FsPath::new(".tmp").join(&tmp_name))?;
    if let Some(parent) = tmp_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError(EngineError::Value(format!(
                "failed to store upload: {error}"
            )))
        })?;
    }
    let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|error| {
        ApiError(EngineError::Value(format!(
            "failed to store upload: {error}"
        )))
    })?;
    let mut hasher = Sha256::new();
    let mut size_bytes: i64 = 0;
    let mut magic: Vec<u8> = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|error| {
        ApiError(EngineError::Value(format!(
            "failed to store upload: {error}"
        )))
    })? {
        if magic.len() < MAGIC_BYTES_LIMIT {
            let remaining = MAGIC_BYTES_LIMIT - magic.len();
            magic.extend(chunk.iter().take(remaining).copied());
        }
        hasher.update(&chunk);
        size_bytes = size_bytes.saturating_add(i64::try_from(chunk.len()).unwrap_or(i64::MAX));
        tokio::io::AsyncWriteExt::write_all(&mut file, chunk.as_ref())
            .await
            .map_err(|error| {
                ApiError(EngineError::Value(format!(
                    "failed to store upload: {error}"
                )))
            })?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|error| {
            ApiError(EngineError::Value(format!(
                "failed to store upload: {error}"
            )))
        })?;
    drop(file);
    Ok(StoredUpload {
        tmp_path,
        upload_root: upload_root.to_path_buf(),
        safe_filename,
        sha256: format!("{:x}", hasher.finalize()),
        size_bytes,
        magic,
        content_type: field.content_type().map(str::to_string),
        original_filename,
    })
}

fn safe_child(root: &FsPath, relative: &FsPath) -> Result<PathBuf, ApiError> {
    let candidate = mission_workspace::resolved_path(&root.join(relative));
    match candidate.strip_prefix(root) {
        Ok(_) => Ok(candidate),
        Err(_) => Err(escape_error()),
    }
}

fn escape_error() -> ApiError {
    ApiError(EngineError::Value(
        "upload path escapes upload root".to_string(),
    ))
}

fn collision_safe_path(
    upload_root: &FsPath,
    directory: &FsPath,
    filename: &str,
) -> Result<PathBuf, ApiError> {
    let directory = mission_workspace::resolved_path(&upload_root.join(directory));
    let relative_directory = directory
        .strip_prefix(upload_root)
        .map_err(|_| escape_error())?;
    let candidate = safe_child(upload_root, &relative_directory.join(filename))?;
    if !candidate.exists() {
        return Ok(candidate);
    }
    let path = FsPath::new(filename);
    let stem = path.file_stem().map_or_else(
        || "upload".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let suffix = path.extension().map_or_else(String::new, |extension| {
        format!(".{}", extension.to_string_lossy())
    });
    for index in 1..10_000 {
        let candidate = safe_child(
            upload_root,
            &relative_directory.join(format!("{stem}-{index}{suffix}")),
        )?;
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ApiError(EngineError::Value(
        "could not allocate upload filename".to_string(),
    )))
}

/// `shutil.move` 镜像：优先原子 rename，跨设备回退复制后删除。
async fn move_into_place(tmp_path: &FsPath, final_path: &FsPath) -> Result<(), ApiError> {
    if tokio::fs::rename(tmp_path, final_path).await.is_ok() {
        return Ok(());
    }
    {
        let bytes = tokio::fs::read(tmp_path).await.map_err(|error| {
            ApiError(EngineError::Value(format!(
                "failed to finalize upload: {error}"
            )))
        })?;
        tokio::fs::write(final_path, &bytes)
            .await
            .map_err(|error| {
                ApiError(EngineError::Value(format!(
                    "failed to finalize upload: {error}"
                )))
            })?;
        let _ = tokio::fs::remove_file(tmp_path).await;
        Ok(())
    }
}

fn file_name(path: &FsPath) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn map_workspace_error(error: &MissionWorkspaceError) -> ApiError {
    ApiError(EngineError::Value(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use engines::default_solver_registry;
    use runtime::InMemoryTaskBackend;
    use runtime::mission_lifecycle::CreateMissionInput;
    use storage::SqliteRepository;
    use tower::ServiceExt;

    use super::*;
    use crate::ApiState;

    /// 表单字段：`(name, filename, content_type, body)`。
    type MultipartField<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

    /// 组装 multipart/form-data 请求体（字段按序拼接，file 字段带
    /// filename 与 content-type）。
    fn multipart_request(
        uri: &str,
        boundary: &str,
        fields: &[MultipartField<'_>],
    ) -> Request<Body> {
        let mut body: Vec<u8> = Vec::new();
        for (name, filename, content_type, payload) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            match filename {
                Some(filename) => body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n"
                    )
                    .as_bytes(),
                ),
                None => body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
                ),
            }
            if let Some(content_type) = content_type {
                body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(payload);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .expect("request must build")
    }

    fn test_state(upload_root: &Path, workspace_root: &Path) -> ApiState {
        let repository =
            Arc::new(SqliteRepository::open(":memory:").expect("in-memory repository must open"));
        let manager = Arc::new(runtime::AuditManager::new(
            repository,
            default_solver_registry(),
            Arc::new(InMemoryTaskBackend::default()),
        ));
        let execution_control = Arc::new(
            runtime::ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
                .expect("execution control must initialize"),
        );
        let tool_installs = Arc::new(
            engines::tool_catalog::ToolInstallCoordinator::new(
                workspace_root.join("local-tools.json"),
            )
            .expect("tool coordinator must initialize"),
        );
        ApiState::with_services(
            manager,
            execution_control,
            tool_installs,
            workspace_root.join("local-tools.json"),
            upload_root.to_path_buf(),
            workspace_root.to_path_buf(),
        )
    }

    async fn body_bytes(response: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body must read");
        serde_json::from_slice(&bytes).expect("body must parse")
    }

    #[tokio::test]
    async fn upload_registers_artifact_metadata_and_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = test_state(
            &directory.path().join("uploads"),
            &directory.path().join("workspace").join("missions"),
        );
        let app = crate::router(state);
        let content = b"login failed\n";

        let response = app
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[("file", Some("evidence.log"), Some("text/plain"), content)],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_bytes(response).await;
        assert_eq!(body["detected_input_type"], "evidence_document");
        assert_eq!(body["detected_target_type"], "mixed");
        assert_eq!(
            body["sha256"],
            format!("{:x}", sha2::Sha256::digest(content))
        );
        assert_eq!(body["size_bytes"], serde_json::json!(content.len()));
        assert_eq!(body["artifact"]["kind"], "log");
        assert_eq!(body["artifact"]["mime_type"], "text/plain");
        assert_eq!(body["artifact"]["storage_backend"], "local_filesystem");
        assert_eq!(
            body["artifact"]["metadata"]["original_filename"],
            "evidence.log"
        );
        assert_eq!(
            body["artifact"]["metadata"]["stored_filename"],
            "evidence.log"
        );
        let uri = body["artifact"]["uri"].as_str().expect("uri");
        let saved = std::fs::canonicalize(uri).expect("artifact file must exist");
        let upload_root = std::fs::canonicalize(directory.path().join("uploads"))
            .expect("upload root must exist");
        assert!(saved.starts_with(upload_root));
        assert!(
            uri.contains("staging"),
            "unbound uploads land in staging: {uri}"
        );
    }

    #[tokio::test]
    async fn upload_sanitizes_path_traversal_filename() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = test_state(
            &directory.path().join("uploads"),
            &directory.path().join("workspace").join("missions"),
        );
        let app = crate::router(state);
        let response = app
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[(
                    "file",
                    Some("..\\..\\secret.json"),
                    Some("application/json"),
                    b"{}".as_slice(),
                )],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_bytes(response).await;
        assert_eq!(
            body["artifact"]["metadata"]["stored_filename"],
            "secret.json"
        );
        let uri = body["artifact"]["uri"].as_str().expect("uri");
        assert!(!uri.contains(".."), "stored path must not traverse: {uri}");
    }

    #[tokio::test]
    async fn upload_classification_table_matches_python() {
        let cases: [(&str, &[u8], &str, &str); 10] = [
            ("src.zip", b"PK\x03\x04payload", "source_archive", "source"),
            ("sample.exe", b"MZpayload", "binary", "binary"),
            (
                "capture.pcap",
                b"\xd4\xc3\xb2\xa1payload",
                "traffic_capture",
                "traffic",
            ),
            ("traffic.har", b"{}", "traffic_capture", "traffic"),
            ("cases.csv", b"a,b\n1,2\n", "investigation_dataset", "mixed"),
            (
                "report.xlsx",
                b"PK\x03\x04payload",
                "investigation_dataset",
                "mixed",
            ),
            ("router.fw", b"firmware", "firmware_image", "binary"),
            ("image.squashfs", b"hsqs", "firmware_image", "binary"),
            ("dump.mem", b"\x00\x01mem", "firmware_image", "binary"),
            ("unknown.weird", b"payload", "unknown_artifact", "unknown"),
        ];
        for (index, (filename, content, expected_input, expected_target)) in
            cases.into_iter().enumerate()
        {
            let directory = tempfile::tempdir().expect("tempdir");
            let state = test_state(
                &directory.path().join("uploads"),
                &directory.path().join("workspace").join("missions"),
            );
            let app = crate::router(state);
            let response = app
                .oneshot(multipart_request(
                    "/uploads",
                    "multipart-boundary",
                    &[(
                        "file",
                        Some(filename),
                        Some("application/octet-stream"),
                        content,
                    )],
                ))
                .await
                .expect("router must respond");
            assert_eq!(response.status(), StatusCode::CREATED, "{filename}");
            let body = body_bytes(response).await;
            assert_eq!(body["detected_input_type"], expected_input, "{filename}");
            assert_eq!(body["detected_target_type"], expected_target, "{filename}");
            assert_eq!(
                body["artifact"]["metadata"]["detected_input_type"], expected_input,
                "{filename} case {index}"
            );
            assert_eq!(
                body["artifact"]["metadata"]["detected_target_type"],
                expected_target
            );
        }
    }

    #[tokio::test]
    async fn upload_with_mission_id_creates_workspace_and_asset() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = test_state(
            &directory.path().join("uploads"),
            &directory.path().join("workspace").join("missions"),
        );
        let mut target = Map::new();
        target.insert(
            "raw_prompt".to_string(),
            Value::String("Analyze packet capture".to_string()),
        );
        let mission = state
            .manager
            .create_mission(CreateMissionInput {
                user_goal: "Analyze packet capture".to_string(),
                title: None,
                target: Some(target),
                project_id: None,
                constraints: Vec::new(),
                success_criteria: Vec::new(),
                goal_contract: None,
                tags: Vec::new(),
                category: None,
                approval_mode: models::ApprovalMode::AskForApproval,
                created_by: String::new(),
                metadata: Map::new(),
            })
            .await
            .expect("mission must create");

        let app = crate::router(state.clone());
        let response = app
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[
                    ("mission_id", None, None, mission.id.as_str().as_bytes()),
                    (
                        "file",
                        Some("capture.pcap"),
                        Some("application/vnd.tcpdump.pcap"),
                        b"\xd4\xc3\xb2\xa1payload".as_slice(),
                    ),
                ],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_bytes(response).await;
        let artifact = &body["artifact"];
        let uri = artifact["uri"].as_str().expect("uri");
        let saved = std::fs::canonicalize(uri).expect("stored file");
        let workspace_root =
            std::fs::canonicalize(directory.path().join("workspace").join("missions"))
                .expect("workspace root");
        assert!(saved.starts_with(workspace_root));
        assert_eq!(
            saved.parent().expect("parent").file_name().unwrap(),
            "uploads"
        );
        assert_eq!(artifact["metadata"]["mission_id"], mission.id.as_str());
        assert_eq!(
            artifact["metadata"]["workspace_relative_path"],
            "uploads/capture.pcap"
        );
        let workspace_path = artifact["metadata"]["mission_workspace_path"]
            .as_str()
            .expect("workspace path");
        assert!(!workspace_path.is_empty());

        // The mission metadata now carries the workspace fields.
        let refreshed = state
            .manager
            .repository()
            .get_mission(mission.id.as_str())
            .expect("read")
            .expect("mission");
        assert_eq!(
            refreshed.metadata["workspace_path"].as_str(),
            Some(workspace_path)
        );
        assert!(refreshed.metadata["workspace_name"].as_str().is_some());

        // The artifact is bound to the mission as a traffic_capture asset.
        let assets = state
            .manager
            .repository()
            .list_mission_assets(Some(mission.id.as_str()), None, None, None)
            .expect("assets must list");
        assert!(assets.iter().any(|asset| {
            asset.asset_type.as_str() == "traffic_capture"
                && asset.source_id.as_deref() == Some(artifact["id"].as_str().expect("id"))
                && asset.metadata["artifact_record_id"].as_str() == artifact["id"].as_str()
        }));
    }

    #[tokio::test]
    async fn upload_rejects_unknown_references_and_empty_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = test_state(
            &directory.path().join("uploads"),
            &directory.path().join("workspace").join("missions"),
        );
        let app = crate::router(state);

        let response = app
            .clone()
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[
                    (
                        "file",
                        Some("capture.pcap"),
                        Some("application/octet-stream"),
                        b"payload".as_slice(),
                    ),
                    ("mission_id", None, None, b"mission_missing".as_slice()),
                ],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_bytes(response).await;
        assert_eq!(body["detail"], "mission not found: mission_missing");

        let response = app
            .clone()
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[(
                    "file",
                    Some("empty.log"),
                    Some("text/plain"),
                    b"".as_slice(),
                )],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_bytes(response).await;
        assert_eq!(body["detail"], "uploaded file is empty");

        let response = app
            .oneshot(multipart_request(
                "/uploads",
                "multipart-boundary",
                &[("purpose", None, None, b"mission_intake".as_slice())],
            ))
            .await
            .expect("router must respond");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
