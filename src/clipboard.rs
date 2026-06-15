//! System clipboard copy for the smolcode TUI (leader `y` yanks the last
//! message/transcript). std-only: shells out to the first available clipboard
//! tool, feeding text via stdin. Backends, in order: `wl-copy` (Wayland),
//! `xclip` (X11), `xsel` (X11 alt), `pbcopy` (macOS).

use std::env;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Clipboard backends to try, in order: command name + its args.
const BACKENDS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("pbcopy", &[]),
];

/// Copy `text` to the system clipboard using the first available backend.
/// Returns Ok(backend_name) on success, or Err(reason) if no backend worked.
pub fn copy(text: &str) -> Result<&'static str, String> {
    for (cmd, args) in BACKENDS {
        if !on_path(cmd) {
            continue;
        }
        match try_copy(cmd, args, text) {
            Ok(true) => return Ok(cmd),
            // Spawned but failed, or binary vanished between check and spawn:
            // fall through to the next backend.
            Ok(false) | Err(_) => continue,
        }
    }
    Err(
        "no clipboard tool found; install wl-clipboard / xclip / xsel"
            .to_string(),
    )
}

/// The clipboard backend that would be used, if any (for diagnostics/UI).
#[allow(dead_code)] // public API: surfaced in diagnostics
pub fn backend() -> Option<&'static str> {
    BACKENDS
        .iter()
        .find(|(cmd, _)| on_path(cmd))
        .map(|(cmd, _)| *cmd)
}

/// Spawn `cmd args`, write `text` to its stdin, and report whether it
/// exited successfully.
fn try_copy(cmd: &str, args: &[&str], text: &str) -> std::io::Result<bool> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
        // Drop closes stdin so the child sees EOF and can exit.
        drop(stdin);
    }

    let status = child.wait()?;
    Ok(status.success())
}

/// Cheap PATH lookup: is `cmd` an existing file in one of the PATH dirs?
fn on_path(cmd: &str) -> bool {
    let path = match env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    env::split_paths(&path).any(|dir| {
        let candidate = if dir.as_os_str().is_empty() {
            Path::new(cmd).to_path_buf()
        } else {
            dir.join(cmd)
        };
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_path_returns_bool_without_panicking() {
        // Just exercising the code path; either answer is acceptable.
        let _ = on_path("wl-copy");
    }

    #[test]
    fn on_path_finds_a_ubiquitous_binary() {
        // `sh` exists on every unix CI runner.
        if cfg!(unix) {
            assert!(on_path("sh"));
        }
    }

    #[test]
    fn on_path_rejects_gibberish() {
        assert!(!on_path("definitely-not-a-real-binary-zzqx-9182"));
    }

    #[test]
    fn backend_returns_option_without_panicking() {
        let _ = backend();
    }

    #[test]
    fn copy_empty_does_not_panic() {
        // CI may lack any clipboard tool, so accept either outcome.
        let result = copy("");
        match result {
            Ok(name) => assert!(!name.is_empty()),
            Err(reason) => assert!(!reason.is_empty()),
        }
    }
}
