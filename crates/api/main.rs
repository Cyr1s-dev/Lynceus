//! Lynceus Rust API 服务入口。
//!
//! 配置通过环境变量提供，便于 Tauri sidecar 与裸二进制复用：
//! `LYNCEUS_DB` 指向 SQLite 文件，`LYNCEUS_BIND` 指向监听地址。默认值
//! 是当前目录下的 `data/lynceus.db` 与 `127.0.0.1:8000`。

use std::env;
use std::sync::Arc;

use axum::Router;
use storage::{Repository, SqliteRepository};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database = env::var("LYNCEUS_DB").unwrap_or_else(|_| "data/lynceus.db".to_string());
    let bind = env::var("LYNCEUS_BIND").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
    let repository = Arc::new(SqliteRepository::open(database)?);
    // Legacy 知识迁移（§39，确定性，不依赖 LLM）：失败不阻塞启动——
    // 服务照常提供，但 /knowledge/index-status 会如实显示 stale。
    match storage::migrate_legacy_cards(repository.as_ref()) {
        Ok(count) if count > 0 => {
            eprintln!("migrated {count} legacy knowledge cards");
            if let Err(error) = repository.sync_knowledge_index() {
                eprintln!("knowledge index sync after migration failed: {error}");
            }
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("legacy knowledge migration skipped: {error}");
        }
    }
    // 唯一 production composition 同时装配 solver/task backend 和两个
    // provider runtime，避免 api.exe 与集成测试出现不同 wiring。
    // 先克隆一份留给网关自动拉起用：`build_production_manager` 按值取走
    // 原 Arc，之后再用即 use-after-move。
    let gateway_repository = Arc::clone(&repository);
    let manager = api::build_production_manager(repository)?;
    let state = api::ApiState::new(manager)?;
    let _recovered = state.execution_control.recover_orphaned()?;
    // 网关句柄先取：`state` 随后被 move 进 `api::router`。
    let gateway_manager = Arc::clone(&state.gateway);
    let app: Router = api::router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    // Startup 后台工具探测：HTTP server 立即 ready，PATH 扫描/身份
    // 校验在后台完成后写磁盘快照。`GET /tool-catalog` 永远只读
    // 快照（首次请求在快照就绪前如实显示 Unknown，由前端提示
    // 「后台检测中」），绝不因 list 触发同步探测。
    {
        let local_tools = engines::tool_catalog::local_tools_config_path();
        let snapshot = engines::tool_catalog::detection_snapshot_path_for(&local_tools);
        tokio::spawn(async move {
            if let Err(error) =
                engines::tool_catalog::refresh_detection_snapshot(&local_tools, &snapshot).await
            {
                eprintln!("tool catalog startup detection failed: {error}");
            }
        });
    }
    // 网关自动拉起：`gateway.yaml` 声明 `enabled: true` 时，api 一起床就把
    // LiteLLM sidecar 拉起来。否则每次 api 重启（build-local / 崩溃恢复）后
    // 所有 worker 都挂在「网关未启动」上，必须人工去 Gateway 页面点启动——
    // 这正是反复出现的老问题。已在跑（含收养的外部网关）时尊重现状；
    // 失败只告警不阻塞：HTTP 服务照常 ready。
    if env::var("LYNCEUS_GATEWAY_AUTOSTART").as_deref() != Ok("0") {
        let gateway = gateway_manager;
        let repository = gateway_repository;
        tokio::spawn(async move {
            if gateway.status().running {
                return;
            }
            match gateway.start_with_repository(repository).await {
                Ok(status) if status.running => eprintln!(
                    "gateway auto-started on port {}",
                    status.port.unwrap_or_default()
                ),
                Ok(_) => {}
                // 没配网关是正常状态，不必每次启动都刷一行告警。
                Err(error) if error.contains("not enabled") => {}
                Err(error) => eprintln!("gateway auto-start failed: {error}"),
            }
        });
    }
    axum::serve(listener, app).await?;
    Ok(())
}
