#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use tauri::{Manager, RunEvent};

fn api_binary(app: &tauri::App) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        let name = if cfg!(windows) {
            "api.exe"
        } else {
            "api"
        };
        return Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/debug")
            .join(name));
    }
    let name = if cfg!(windows) {
        "api.exe"
    } else {
        "api"
    };
    Ok(app.path().resource_dir()?.join("bin").join(name))
}

fn spawn_api(app: &tauri::App) -> Result<Child, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?.join("data");
    let logs = data_dir.join("logs");
    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(data_dir.join("config"))?;
    fs::create_dir_all(&logs)?;

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("api.log"))?;
    let resource_dir = app.path().resource_dir()?;
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runtime_root = if cfg!(debug_assertions) {
        &source_root
    } else {
        &resource_dir
    };
    let mut command = Command::new(api_binary(app)?);
    command
        .current_dir(runtime_root)
        .env("LYNCEUS_BIND", "127.0.0.1:8000")
        .env("LYNCEUS_DB", data_dir.join("lynceus.db"))
        .env("LYNCEUS_WORKSPACE_DIR", &data_dir)
        .env(
            "LYNCEUS_LOCAL_TOOLS_CONFIG",
            data_dir.join("config/local-tools.json"),
        )
        .env(
            "LYNCEUS_FINGERPRINT_PACKS_DIR",
            runtime_root.join("resources/fingerprints"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    hide_console_window(&mut command);
    Ok(command.spawn()?)
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

fn stop_api(child: &Arc<Mutex<Option<Child>>>) -> io::Result<()> {
    let mut child = child
        .lock()
        .map_err(|_| io::Error::other("API process lock poisoned"))?;
    if let Some(mut process) = child.take() {
        process.kill()?;
        process.wait()?;
    }
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        .build(tauri::generate_context!())
        .expect("error while building Lynceus desktop shell");
    let api = Arc::new(Mutex::new(Some(
        spawn_api(&app).expect("error while starting Lynceus Rust API"),
    )));
    app.run(move |_app, event| {
        if matches!(event, RunEvent::Exit) {
            let _ = stop_api(&api);
        }
    });
}
