//! LocalSend handoff: open a file in the locally installed LocalSend so the
//! user picks a target in its own UI.
//!
//! Atoll's integration precedent — Arc does not implement the LocalSend
//! protocol itself (UDP discovery + upload negotiation), it only locates the
//! installed executable and passes file arguments. LocalSend's send window
//! opens with those files staged; everything else is LocalSend's job.
//!
//! Detection is a fixed candidate list, not a registry scan: LocalSend's
//! Windows installer has two known locations and the app has no documented
//! CLI beyond file arguments.

use std::path::PathBuf;

/// Candidate install locations, in order of likelihood.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    // `%ProgramFiles%\LocalSend` is where the installer actually puts it on
    // this machine; `%LOCALAPPDATA%\Programs\localsend` is the other known
    // location.
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        out.push(PathBuf::from(pf).join("LocalSend").join("localsend_app.exe"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(PathBuf::from(local).join("Programs").join("localsend").join("localsend_app.exe"));
    }
    if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
        out.push(PathBuf::from(pf86).join("LocalSend").join("localsend_app.exe"));
    }
    out
}

/// The installed LocalSend executable, if any.
pub fn detect() -> Option<PathBuf> {
    candidates().into_iter().find(|p| p.is_file())
}

/// Hand `files` to LocalSend.
///
/// Spawns detached: the send window is LocalSend's, it must outlive this
/// call, and failing to launch must not touch Arc. Returns whether the
/// launch itself was attempted successfully.
pub fn handoff(files: &[PathBuf]) -> bool {
    if files.is_empty() {
        return false;
    }
    let Some(app) = detect() else {
        return false;
    };
    let mut cmd = std::process::Command::new(&app);
    cmd.args(files);
    // Detached: don't hold LocalSend's lifetime tied to ours, and don't block
    // waiting for its window to close.
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    cmd.spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_files_means_no_launch() {
        // Never touches the filesystem beyond an empty-args early return.
        assert!(!handoff(&[]));
    }

    #[test]
    fn detect_only_returns_paths_that_exist() {
        if let Some(p) = detect() {
            assert!(p.is_file(), "detect returned a missing file: {}", p.display());
            assert!(
                p.file_name().map(|n| n == "localsend_app.exe").unwrap_or(false),
                "unexpected exe name: {}",
                p.display()
            );
        }
    }

    #[test]
    fn candidates_are_absolute_exe_paths() {
        for c in candidates() {
            assert!(c.is_absolute(), "{}", c.display());
            assert!(c.extension().map(|e| e == "exe").unwrap_or(false));
        }
    }
}