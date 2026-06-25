#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::project_registry::{validate_project_paths, ProjectSession};
use crate::vs_registry::AppSettings;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub struct OpenVsProcess {
    pub process_id: u32,
    pub devenv_path: String,
}

pub fn open_visual_studio_process(
    project: &ProjectSession,
    settings: &AppSettings,
) -> Result<OpenVsProcess, String> {
    validate_project_paths(project)?;
    let solution_path = project
        .solution_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            "This project has no solutionPath. Add a .sln before using Visual Studio bridge features."
                .to_string()
        })?;

    if let Some(existing) = find_existing_vs_instance(project) {
        return Ok(existing);
    }

    let devenv_path = resolve_devenv_path(settings)?;
    let child = Command::new(&devenv_path)
        .arg(solution_path)
        .spawn()
        .map_err(|error| {
            format!(
                "启动 Visual Studio 失败 {} {}: {error}",
                devenv_path, solution_path
            )
        })?;

    Ok(OpenVsProcess {
        process_id: child.id(),
        devenv_path,
    })
}

pub fn resolve_devenv_path(settings: &AppSettings) -> Result<String, String> {
    if let Some(path) = settings.devenv_path.as_deref() {
        if !path.trim().is_empty() {
            if Path::new(path).is_file() {
                return Ok(path.to_string());
            }
            return Err(format!("devenv.exe does not exist: {path}"));
        }
    }

    if let Some(path) = find_devenv_with_vswhere() {
        return Ok(path);
    }

    let candidates = [
        r"C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\devenv.exe",
        r"C:\Program Files\Microsoft Visual Studio\18\Professional\Common7\IDE\devenv.exe",
        r"C:\Program Files\Microsoft Visual Studio\18\Enterprise\Common7\IDE\devenv.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\devenv.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Professional\Common7\IDE\devenv.exe",
        r"C:\Program Files\Microsoft Visual Studio\2022\Enterprise\Common7\IDE\devenv.exe",
    ];

    candidates
        .iter()
        .find(|path| Path::new(path).is_file())
        .map(|path| path.to_string())
        .ok_or_else(|| {
            "devenv.exe does not exist. Configure the Visual Studio devenv.exe path in Settings."
                .to_string()
        })
}

pub fn find_existing_vs_instance(_project: &ProjectSession) -> Option<OpenVsProcess> {
    None
}

fn find_devenv_with_vswhere() -> Option<String> {
    let vswhere =
        Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }

    let mut command = Command::new(vswhere);
    hide_child_console(&mut command);
    let output = command
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "productPath",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() || !Path::new(&path).is_file() {
        return None;
    }
    Some(path)
}

fn hide_child_console(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}
