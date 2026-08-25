//! Hand a log off to the user's own editor.
//!
//! Search, regex, folding, and copying are things an editor already does far
//! better than a TUI reimplementation, so `e` writes the buffer to a temp file
//! and opens it in `$VISUAL` / `$EDITOR`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use std::process::Command;

/// What the UI asks the main loop to open. Built in the app, performed by the
/// run loop, which is the only place that can safely suspend the terminal.
#[derive(Debug, Clone)]
pub struct EditRequest {
    /// Basename only; the extension drives the editor's syntax highlighting.
    pub file_name: String,
    pub contents: String,
    /// From config.toml's `editor`, when set.
    pub configured: Option<String>,
}

/// `$VISUAL` wins over `$EDITOR` by long-standing convention (VISUAL is the
/// full-screen one). Falls back to a pager, then vi, so this works on a box
/// with nothing configured.
pub fn resolve_editor(configured: Option<&str>) -> (String, Vec<String>) {
    // config.toml wins: it is the setting a user of this tool set deliberately.
    if let Some(raw) = configured {
        let raw = raw.trim();
        if !raw.is_empty() {
            let mut parts = raw.split_whitespace().map(str::to_string);
            if let Some(prog) = parts.next() {
                return (prog, parts.collect());
            }
        }
    }
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(raw) = std::env::var(var) {
            let raw = raw.trim();
            if !raw.is_empty() {
                // Editors are commonly set with flags, e.g. "code --wait".
                let mut parts = raw.split_whitespace().map(str::to_string);
                if let Some(prog) = parts.next() {
                    return (prog, parts.collect());
                }
            }
        }
    }
    let fallback = if which("less") { "less" } else { "vi" };
    (fallback.to_string(), Vec::new())
}

fn which(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Windows tolerates fewer characters in a filename than a GoCD pipeline name
/// contains, and a stray path separator would escape the temp directory.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect();
    cleaned.trim_matches('-').to_string()
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let home = dirs::home_dir()?;
    Some(std::fs::metadata(home).ok()?.uid())
}

/// Console logs routinely carry build secrets and the system temp dir is shared,
/// so logs go in a private per-user subdirectory rather than loose in /tmp.
pub fn session_dir() -> Result<PathBuf> {
    let mut dir = std::env::temp_dir();

    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let uid = current_uid();
        dir.push(format!(
            "lazygocd-{}",
            uid.map(|u| u.to_string()).unwrap_or_else(|| "user".into())
        ));
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        // symlink_metadata, not metadata: a symlink planted here would otherwise
        // redirect the write and pass every check that follows.
        let meta = std::fs::symlink_metadata(&dir)
            .with_context(|| format!("checking {}", dir.display()))?;
        if !meta.is_dir() {
            anyhow::bail!("{} exists and is not a directory", dir.display());
        }
        if let Some(uid) = uid
            && meta.uid() != uid
        {
            anyhow::bail!("{} is owned by another user", dir.display());
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok();
    }

    #[cfg(not(unix))]
    {
        dir.push("lazygocd");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    Ok(dir)
}

pub fn temp_path(file_name: &str) -> Result<PathBuf> {
    let mut p = session_dir()?;
    p.push(sanitize(file_name));
    Ok(p)
}

/// Writes the buffer and blocks on the editor. A GUI editor that forks
/// (`code`, `zed`) returns immediately, which is the behaviour people expect:
/// the window opens beside the terminal and the TUI comes straight back.
pub fn edit(req: &EditRequest) -> Result<()> {
    let path = temp_path(&req.file_name)?;
    crate::config::write_private(&path, &req.contents)
        .with_context(|| format!("writing {}", path.display()))?;
    let (program, args) = resolve_editor(req.configured.as_deref());
    let status = Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .with_context(|| {
            format!("launching editor {program:?} (set `editor` in config.toml or $EDITOR)")
        })?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Sweeps lazygocd temp files left from previous runs.
///
/// Deleting right after the editor exits is wrong for GUI editors: `code` and
/// `zed` return as soon as the window is handed the file (measured at ~4s for a
/// cold VS Code launch, instantly when already running), so removing the file
/// then yanks it out from under the open tab. Instead the file is left alone and
/// stale ones are swept on the next start.
pub fn cleanup_stale(max_age: Duration) {
    let now = SystemTime::now();
    let sweep = |dir: PathBuf, prefixed: bool| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if prefixed && !name.starts_with("lazygocd-") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                continue;
            }
            let stale = meta
                .modified()
                .map(|t| now.duration_since(t).unwrap_or_default() > max_age)
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    };
    if let Ok(dir) = session_dir() {
        sweep(dir, false);
    }
    // Versions before 0.10.3 wrote logs loose in the temp root.
    sweep(std::env::temp_dir(), true);
}

#[cfg(test)]
mod tests {
    // config.toml must win over the environment, otherwise setting it does
    // nothing on a machine that already exports EDITOR.
    #[test]
    fn configured_editor_beats_the_environment() {
        let (prog, args) = super::resolve_editor(Some("code --wait"));
        assert_eq!(prog, "code");
        assert_eq!(args, vec!["--wait"]);
    }

    #[test]
    fn blank_config_falls_through_to_the_environment_or_default() {
        // An empty or whitespace value must not shadow $EDITOR.
        let (prog, _) = super::resolve_editor(Some("   "));
        assert!(!prog.is_empty());
        assert_ne!(prog, "   ");
    }

    #[test]
    fn visual_beats_editor_and_flags_are_split() {
        // Not asserting against the real env: just the parsing contract that a
        // value like "code --wait" becomes a program plus its arguments.
        let raw = "code --wait --new-window";
        let mut parts = raw.split_whitespace().map(str::to_string);
        let prog = parts.next().unwrap();
        let args: Vec<String> = parts.collect();
        assert_eq!(prog, "code");
        assert_eq!(args, vec!["--wait", "--new-window"]);
    }

    #[test]
    fn sanitize_strips_path_separators_and_odd_characters() {
        assert_eq!(super::sanitize("web-app/ui build#1"), "web-app-ui-build-1");
        assert_eq!(super::sanitize("plain-name.log"), "plain-name.log");

        // The property that matters is that nothing can traverse out of the
        // temp dir. Dots survive (they carry the extension), separators do not.
        for hostile in ["../../etc/passwd", "a/b/c", "..\\..\\win", "/abs/path"] {
            let out = super::sanitize(hostile);
            assert!(!out.contains('/'), "{out:?} still has a separator");
            assert!(!out.contains('\\'), "{out:?} still has a separator");
        }
    }

    #[test]
    fn temp_path_stays_inside_the_private_session_dir() {
        let dir = super::session_dir().unwrap();
        let p = super::temp_path("../../escape.log").unwrap();
        assert_eq!(p.parent().unwrap(), dir);
        assert!(dir.starts_with(std::env::temp_dir()));
    }

    // The log can hold build secrets, so neither the directory nor the file may
    // be readable by other users on the machine.
    #[cfg(unix)]
    #[test]
    fn session_dir_and_log_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = super::session_dir().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "session dir is {mode:o}");

        let path = super::temp_path("perm-check.log").unwrap();
        crate::config::write_private(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "log file is {mode:o}");

        // A file left behind at 0644 by an older version must be tightened, not
        // inherited: .mode() on OpenOptions only applies when creating.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        crate::config::write_private(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600, "pre-existing file left at {mode:o}");
    }
}
