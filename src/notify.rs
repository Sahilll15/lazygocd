//! Best-effort desktop notifications with zero extra dependencies: osascript
//! on macOS, notify-send on Linux, silent no-op anywhere else.

/// Fire-and-forget from the UI thread; the subprocess runs on its own thread.
pub fn notify(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || send(&title, &body));
}

fn send(title: &str, body: &str) {
    // Test hook: append "title|body" to this file instead of notifying, so the
    // transition detection is verifiable headless.
    if let Ok(path) = std::env::var("LAZYGOCD_NOTIFY_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{title}|{body}");
        }
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &applescript(title, body)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send")
            .arg(title)
            .arg(body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = (title, body);
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn applescript(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(title)
    )
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_command_construction() {
        assert_eq!(
            applescript("lazygocd", "Pipeline web-app failed"),
            r#"display notification "Pipeline web-app failed" with title "lazygocd""#
        );
    }

    #[test]
    fn applescript_escapes_quotes_and_backslashes() {
        assert_eq!(
            applescript("t", r#"say "hi" \ bye"#),
            r#"display notification "say \"hi\" \\ bye" with title "t""#
        );
    }
}
