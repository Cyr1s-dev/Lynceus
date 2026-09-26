//! `knowledge-import` —— 结构化安全 Wiki → Lynceus 知识库导入 CLI。
//!
//! ```text
//! cargo run -p tools --bin knowledge-import -- \
//!   --db D:\data\lynceus.sqlite3 \
//!   --dir resources\knowledge
//! ```
//!
//! 可选：`--dry-run` 只统计不写入；`--no-sync` 跳过导入后的索引重建
//! （默认导入完自动 `sync_knowledge_index` 收口）。
//!
//! 幂等：确定性 id + `content_hash`，重复导入跳过未变化条目；可重复执行。

use std::fmt;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use storage::Repository;
use tools::ImportSummary;

struct Args {
    db: PathBuf,
    dir: PathBuf,
    dry_run: bool,
    no_sync: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut db = None;
    let mut dir = None;
    let mut dry_run = false;
    let mut no_sync = false;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--db" => {
                db = Some(iter.next().ok_or("--db requires a path")?);
            }
            "--dir" => {
                dir = Some(iter.next().ok_or("--dir requires a path")?);
            }
            "--dry-run" => dry_run = true,
            "--no-sync" => no_sync = true,
            "--help" | "-h" => {
                std::process::exit(i32::from(print_help().is_err()));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        db: PathBuf::from(db.ok_or("--db <path> is required")?),
        dir: PathBuf::from(dir.ok_or("--dir <path> is required")?),
        dry_run,
        no_sync,
    })
}

fn write_stdout(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_fmt(arguments)?;
    stdout.write_all(b"\n")
}

fn print_help() -> io::Result<()> {
    write_stdout(format_args!(
        "knowledge-import --db <lynceus.sqlite3> --dir <knowledge-assets> [--dry-run] [--no-sync]\n\n\
         Imports structured security Wiki JSON (webPayloads/intranetPayloads/toolCommands/\n\
         reverseShell) into the Lynceus knowledge store as retrievable KnowledgeUnits.\n\
         Sensitive source files may use .json.asset when local security software\n\
         blocks their raw .json names."
    ))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("error: {error}\n");
            if let Err(output_error) = print_help() {
                eprintln!("error: cannot write help: {output_error}");
            }
            return ExitCode::from(2);
        }
    };
    if !args.dir.is_dir() {
        eprintln!("error: --dir {} is not a directory", args.dir.display());
        return ExitCode::from(2);
    }

    let repository = if args.dry_run {
        None
    } else {
        match storage::SqliteRepository::open(&args.db) {
            Ok(repository) => Some(repository),
            Err(error) => {
                eprintln!("error: cannot open database {}: {error}", args.db.display());
                return ExitCode::from(2);
            }
        }
    };

    // dry-run 语义 = 只做转换与计数（通过临时库落库后丢弃）。
    let summary: ImportSummary = if args.dry_run {
        let scratch = match tempfile_dir() {
            Ok(dir) => dir,
            Err(error) => {
                eprintln!("error: cannot create scratch dir: {error}");
                return ExitCode::from(2);
            }
        };
        let scratch_db = scratch.path().join("dry-run.sqlite3");
        let scratch_repo = match storage::SqliteRepository::open(&scratch_db) {
            Ok(repo) => repo,
            Err(error) => {
                eprintln!("error: cannot open scratch db: {error}");
                return ExitCode::from(2);
            }
        };
        match tools::import_directory(&scratch_repo, &args.dir) {
            Ok(summary) => summary,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        let Some(repository) = repository.as_ref() else {
            eprintln!("error: target database is unavailable");
            return ExitCode::from(2);
        };
        match tools::import_directory(repository, &args.dir) {
            Ok(summary) => summary,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(1);
            }
        }
    };

    if let Err(error) = write_stdout(format_args!(
        "imported: files={} payload={} payload_children={} tool={} command={} shell={} \
         skipped_unchanged={} skipped_invalid={}",
        summary.files,
        summary.payload_units,
        summary.payload_children,
        summary.tool_units,
        summary.command_units,
        summary.shell_units,
        summary.skipped_unchanged,
        summary.skipped_invalid,
    )) {
        eprintln!("error: cannot write summary: {error}");
        return ExitCode::from(1);
    }

    if let Some(repository) = repository.as_ref().filter(|_| !args.no_sync) {
        match repository.sync_knowledge_index() {
            Ok(status) => {
                if let Err(error) = write_stdout(format_args!(
                    "index: state={:?} cards={} indexed={} last_synced={:?}",
                    status.state,
                    status.card_count,
                    status.indexed_count,
                    status.last_synced_at.map(|ts| ts.to_string()),
                )) {
                    eprintln!("error: cannot write index status: {error}");
                    return ExitCode::from(1);
                }
            }
            Err(error) => {
                eprintln!("error: index sync failed: {error}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn tempfile_dir() -> std::io::Result<TempDirGuard> {
    // dry-run 用系统临时目录手搓（避免仅为 dry-run 引入运行时依赖）。
    let base = std::env::temp_dir().join(format!(
        "knowledge_import_dryrun_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&base)?;
    Ok(TempDirGuard { path: base })
}

/// 手搓临时目录守护（Drop 时清理）。
struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
