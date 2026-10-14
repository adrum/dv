//! Finding commands on `PATH`, the way a shell would.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::shims;

/// First executable `command` on `PATH`.
pub fn on_path(command: &str) -> Option<PathBuf> {
    entries().into_iter().find_map(|dir| find_in(&dir, command))
}

/// First executable `command` on `PATH` that is not one of dv's own shims.
///
/// Every shim ends in `dv run <command>`, so a search that accepted one would
/// hand `dv run` back to itself and spin forever. Identity is the only
/// reliable test — a shim is a real file with a real name, and the marker line
/// is what distinguishes it.
pub fn on_path_excluding_shims(command: &str) -> Result<Option<PathBuf>> {
    for dir in entries() {
        let Some(candidate) = find_in(&dir, command) else {
            continue;
        };
        if shims::is_dv_shim(&candidate)? {
            continue;
        }
        return Ok(Some(candidate));
    }
    Ok(None)
}

pub fn entries() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// The file in `dir` that typing `command` would run, if there is one.
#[cfg(not(windows))]
pub fn find_in(dir: &Path, command: &str) -> Option<PathBuf> {
    Some(dir.join(command)).filter(|candidate| is_executable(candidate))
}

/// The file in `dir` that typing `command` would run, if there is one.
///
/// Windows resolves a bare command by trying each `PATHEXT` extension in
/// turn, so `dotnet` means `dotnet.exe`, `dotnet.cmd` and so on — and a file
/// called just `dotnet` is not a command at all.
#[cfg(windows)]
pub fn find_in(dir: &Path, command: &str) -> Option<PathBuf> {
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    pathext_names(command, &pathext)
        .into_iter()
        .map(|name| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// What Windows searches for when no `PATHEXT` is set.
#[cfg(any(windows, test))]
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The file names `command` can resolve to, in the order they are tried: the
/// name itself when it already has an extension, then one per `PATHEXT` entry.
#[cfg(any(windows, test))]
fn pathext_names(command: &str, pathext: &str) -> Vec<String> {
    let pathext = if pathext.trim().is_empty() {
        DEFAULT_PATHEXT
    } else {
        pathext
    };
    let mut names = Vec::new();
    if Path::new(command).extension().is_some() {
        names.push(command.to_string());
    }
    for extension in pathext.split(';').map(str::trim) {
        if extension.starts_with('.') {
            names.push(format!("{command}{}", extension.to_ascii_lowercase()));
        }
    }
    names
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Windows has no executable bit: the extension is what makes a file runnable,
/// and callers have already chosen that.
#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        crate::paths::make_executable(path).unwrap();
    }

    #[test]
    fn a_shim_is_never_returned_as_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let shim_dir = dir.path().join("shims");
        let real_dir = dir.path().join("bin");
        let shim = shim_dir.join(shims::shim_file("dotnet"));
        // A real dotnet is a file the platform can run by that name.
        let real = real_dir.join(if cfg!(windows) {
            "dotnet.exe"
        } else {
            "dotnet"
        });
        executable(&shim, &shims::shim_body(Path::new("/opt/dv"), "dotnet"));
        executable(&real, "echo real\n");

        // Both are on PATH, with the shim first — exactly the layout dv's own
        // init produces.
        let path = std::env::join_paths([&shim_dir, &real_dir]).unwrap();
        temp_env_path(&path, || {
            assert_eq!(on_path("dotnet"), Some(shim));
            assert_eq!(on_path_excluding_shims("dotnet").unwrap(), Some(real));
        });
    }

    #[test]
    fn a_bare_command_tries_each_pathext_extension_in_order() {
        assert_eq!(
            pathext_names("dotnet", ".COM;.EXE;.CMD"),
            ["dotnet.com", "dotnet.exe", "dotnet.cmd"]
        );
        // An unset PATHEXT falls back to what Windows itself assumes.
        assert_eq!(
            pathext_names("dotnet", ""),
            ["dotnet.com", "dotnet.exe", "dotnet.bat", "dotnet.cmd"]
        );
        // A name that already carries its extension is tried as written first.
        assert_eq!(pathext_names("dotnet.exe", ".EXE")[0], "dotnet.exe");
    }

    #[test]
    fn nothing_on_path_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        temp_env_path(&path, || {
            assert_eq!(on_path("dotnet"), None);
            assert_eq!(on_path_excluding_shims("dotnet").unwrap(), None);
        });
    }

    /// PATH is process-wide, so these tests run one at a time behind a lock.
    fn temp_env_path(value: &std::ffi::OsStr, body: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());

        let previous = std::env::var_os("PATH");
        // SAFETY: serialized by LOCK, and restored before the guard drops.
        unsafe { std::env::set_var("PATH", value) };
        body();
        match previous {
            Some(previous) => unsafe { std::env::set_var("PATH", previous) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}
