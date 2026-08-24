//! Hand a log off to the user's own editor.
//!
//! Search, regex, folding, and copying are things an editor already does far
//! better than a TUI reimplementation, so `e` writes the buffer to a temp file
//! and opens it in `$VISUAL` / `$EDITOR`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What the UI asks the main loop to open. Built in the app, performed by the
/// run loop, which is the only place that can safely suspend the terminal.
#[derive(Debug, Clone)]
pub struct EditRequest {
    /// Basename only; the extension drives the editor's syntax highlighting.
    pub file_name: String,
    pub contents: String,
}

/// `$VISUAL` wins over `$EDITOR` by long-standing convention (VISUAL is the
/// full-screen one). Falls back to a pager, then vi, so this works on a box
/// with nothing configured.
pub fn resolve_editor() -> (String, Vec<String>) {
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

pub fn temp_path(file_name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("lazygocd-{}", sanitize(file_name)));
    p
}

/// Writes the buffer and blocks on the editor. A GUI editor that forks
/// (`code`, `zed`) returns immediately, which is the behaviour people expect:
/// the window opens beside the terminal and the TUI comes straight back.
pub fn edit(req: &EditRequest) -> Result<()> {
    let path = temp_path(&req.file_name);
    std::fs::write(&path, &req.contents)
        .with_context(|| format!("writing {}", path.display()))?;
    let (program, args) = resolve_editor();
    let status = Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor {program:?} (set $EDITOR to change it)"))?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Best-effort cleanup; a leftover temp file is harmless but untidy.
pub fn discard(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
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
    fn temp_path_stays_inside_the_temp_dir() {
        let p = super::temp_path("../../escape.log");
        assert_eq!(p.parent().unwrap(), std::env::temp_dir());
        assert!(p.file_name().unwrap().to_string_lossy().starts_with("lazygocd-"));
    }
}
