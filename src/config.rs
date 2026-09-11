use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// e.g. "https://gocd.example.com/go" (no trailing slash). Empty means unconfigured.
    #[serde(default)]
    pub server_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Bearer/personal access token, used instead of username+password if set.
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub insecure_skip_verify: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Optional GitHub personal access token, only needed to check private repos'
    /// latest commit against what's deployed. Unset = unauthenticated (public repos only).
    #[serde(default)]
    pub github_token: Option<String>,
    /// GitHub REST API root used for stale-deploy checks. Point this at your
    /// GitHub Enterprise instance's /api/v3 to check repos hosted there.
    #[serde(default = "default_github_api_base")]
    pub github_api_base: String,
    /// Desktop notification when a favorited pipeline's latest run turns Failed.
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// Editor for 'e', e.g. "nvim" or "code --wait". Takes precedence over
    /// $VISUAL and $EDITOR so the tool works on a shell that sets neither.
    #[serde(default)]
    pub editor: Option<String>,
}

fn default_poll_interval() -> u64 {
    30
}

fn default_github_api_base() -> String {
    "https://api.github.com".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server_url: String::new(),
            username: None,
            password: None,
            auth_token: None,
            insecure_skip_verify: false,
            poll_interval_secs: default_poll_interval(),
            github_token: None,
            github_api_base: default_github_api_base(),
            notifications: true,
            editor: None,
        }
    }
}

static CONFIG_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Called once from main with the --config-dir flag, before any path fn runs.
pub fn set_config_dir_override(dir: PathBuf) {
    let _ = CONFIG_DIR_OVERRIDE.set(dir);
}

/// Resolution order: --config-dir flag > $XDG_CONFIG_HOME/lazygocd > ~/.config/lazygocd.
pub fn config_dir() -> Result<PathBuf> {
    resolve_config_dir(
        CONFIG_DIR_OVERRIDE.get().map(PathBuf::as_path),
        std::env::var_os("XDG_CONFIG_HOME"),
        dirs::home_dir(),
    )
}

fn resolve_config_dir(
    flag: Option<&Path>,
    xdg: Option<OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(dir) = flag {
        return Ok(dir.to_path_buf());
    }
    if let Some(xdg) = xdg.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("lazygocd"));
    }
    let home = home.context("could not determine home directory")?;
    Ok(home.join(".config").join("lazygocd"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Local disk cache of the last successful dashboard load, so the next launch
/// can show data instantly instead of waiting on a fresh network round trip.
pub fn dashboard_cache_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("dashboard_cache.json"))
}

/// Starred pipeline names, pinned to the top of the tree regardless of group.
pub fn favorites_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("favorites.json"))
}

/// Marker for a value that should be produced by running a command rather than
/// stored literally, e.g. `auth_token = "{{cmd: op read op://vault/item/field}}"`.
const CMD_PREFIX: &str = "{{cmd:";
const CMD_SUFFIX: &str = "}}";

/// Runs `body` through the shell and returns its trimmed stdout.
///
/// Deliberately shell-interpreted so a config can use quoting, pipes and flags
/// the way it would at a prompt. That makes the config file executable input, so
/// it is only ever read from the user's own config path.
fn run_value_command(body: &str, field: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() {
        anyhow::bail!("{field}: {CMD_PREFIX} ... {CMD_SUFFIX} is empty, expected a command to run");
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let out = std::process::Command::new(&shell)
        .arg("-c")
        .arg(body)
        .output()
        .with_context(|| format!("{field}: running `{body}`"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = stderr.trim();
        let detail = if detail.is_empty() { "no stderr" } else { detail };
        anyhow::bail!("{field}: `{body}` failed ({}): {detail}", out.status);
    }

    // Trailing newlines are near-universal in CLI output and would otherwise end
    // up inside a bearer token, producing a confusing 401 rather than an error.
    let value = String::from_utf8(out.stdout)
        .with_context(|| format!("{field}: `{body}` produced non-UTF-8 output"))?
        .trim()
        .to_string();

    if value.is_empty() {
        anyhow::bail!("{field}: `{body}` succeeded but produced no output");
    }
    Ok(value)
}

/// Returns the value to use for a configured string: either the literal, or the
/// output of the command it wraps. Anything not wrapped is passed through
/// untouched, so existing configs holding plain secrets keep working.
fn resolve_value(raw: &str, field: &str) -> Result<String> {
    let trimmed = raw.trim();
    match trimmed
        .strip_prefix(CMD_PREFIX)
        .and_then(|rest| rest.strip_suffix(CMD_SUFFIX))
    {
        Some(body) => run_value_command(body, field),
        None => Ok(raw.to_string()),
    }
}

fn resolve_opt(slot: &mut Option<String>, field: &str) -> Result<()> {
    if let Some(raw) = slot.as_deref() {
        *slot = Some(resolve_value(raw, field)?);
    }
    Ok(())
}

/// Expands every `{{cmd: ...}}` value in a loaded config.
///
/// Applied to all string-valued settings, not just secrets, so a URL or editor
/// can be derived from the environment too. Runs after the env-var overrides so
/// that `GOCD_TOKEN="{{cmd: ...}}"` works exactly like the file form.
fn resolve_command_values(cfg: &mut Config) -> Result<()> {
    cfg.server_url = resolve_value(&cfg.server_url, "server_url")?;
    cfg.github_api_base = resolve_value(&cfg.github_api_base, "github_api_base")?;
    resolve_opt(&mut cfg.username, "username")?;
    resolve_opt(&mut cfg.password, "password")?;
    resolve_opt(&mut cfg.auth_token, "auth_token")?;
    resolve_opt(&mut cfg.github_token, "github_token")?;
    resolve_opt(&mut cfg.editor, "editor")?;
    Ok(())
}

/// Loads config from the file at `config_path()` if present, otherwise an
/// empty/unconfigured `Config`. There's no separate CLI setup wizard: the
/// TUI itself prompts for connection details (see `app::ReauthForm`) when
/// `server_url` ends up empty here. Env vars always override the file.
pub fn load() -> Result<Config> {
    let path = config_path()?;

    let mut cfg = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config file at {}", path.display()))?;
        toml::from_str(&text)
            .with_context(|| format!("parsing config file at {}", path.display()))?
    } else {
        Config::default()
    };

    if let Ok(v) = std::env::var("GOCD_URL") {
        cfg.server_url = v;
    }
    if let Ok(v) = std::env::var("GOCD_USERNAME") {
        cfg.username = Some(v);
    }
    if let Ok(v) = std::env::var("GOCD_PASSWORD") {
        cfg.password = Some(v);
    }
    if let Ok(v) = std::env::var("GOCD_TOKEN") {
        cfg.auth_token = Some(v);
    }
    if std::env::var("GOCD_INSECURE").is_ok() {
        cfg.insecure_skip_verify = true;
    }
    if let Ok(v) = std::env::var("GITHUB_TOKEN") {
        cfg.github_token = Some(v);
    }

    resolve_command_values(&mut cfg)?;

    Ok(cfg)
}

/// Owner-only from the moment the file exists. `fs::write` followed by a chmod
/// leaves the contents world-readable for the window in between.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
        // .mode() only applies on creation, so an older 0644 file needs this too.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    std::fs::write(path, contents)
}

/// Creates the config directory owner-only. It holds the credential file plus
/// the dashboard cache, which lists every pipeline name on the server.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent).ok();
    }
    let text = toml::to_string_pretty(cfg)?;
    write_private(path, &text)
        .with_context(|| format!("writing config to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_value_passes_through_untouched() {
        assert_eq!(resolve_value("abc123", "auth_token").unwrap(), "abc123");
    }

    #[test]
    fn a_value_that_merely_mentions_cmd_is_not_executed() {
        let raw = "token-{{cmd-like}}-value";
        assert_eq!(resolve_value(raw, "auth_token").unwrap(), raw);
    }

    #[test]
    fn wrapped_command_is_executed_and_trimmed() {
        let out = resolve_value("{{cmd: printf 'secret\n'}}", "auth_token").unwrap();
        assert_eq!(out, "secret");
    }

    #[test]
    fn command_can_use_quoting_and_pipes() {
        let out = resolve_value("{{cmd: echo 'a b' | tr ' ' '-'}}", "auth_token").unwrap();
        assert_eq!(out, "a-b");
    }

    #[test]
    fn failing_command_reports_the_field_and_stderr() {
        let err = resolve_value("{{cmd: echo nope >&2; exit 3}}", "auth_token").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("auth_token"), "{msg}");
        assert!(msg.contains("nope"), "{msg}");
    }

    #[test]
    fn command_producing_no_output_is_an_error_not_an_empty_secret() {
        let err = resolve_value("{{cmd: true}}", "auth_token").unwrap_err();
        assert!(err.to_string().contains("no output"), "{err}");
    }

    #[test]
    fn empty_command_body_is_rejected() {
        let err = resolve_value("{{cmd:   }}", "auth_token").unwrap_err();
        assert!(err.to_string().contains("expected a command"), "{err}");
    }

    #[test]
    fn every_string_field_is_resolved_not_just_the_token() {
        let mut cfg = Config {
            server_url: "{{cmd: echo https://gocd.example.com/go}}".into(),
            username: Some("{{cmd: echo alice}}".into()),
            password: Some("{{cmd: echo pw}}".into()),
            auth_token: Some("{{cmd: echo tok}}".into()),
            github_token: Some("{{cmd: echo ghtok}}".into()),
            editor: Some("{{cmd: echo nvim}}".into()),
            ..Config::default()
        };
        resolve_command_values(&mut cfg).unwrap();
        assert_eq!(cfg.server_url, "https://gocd.example.com/go");
        assert_eq!(cfg.username.as_deref(), Some("alice"));
        assert_eq!(cfg.password.as_deref(), Some("pw"));
        assert_eq!(cfg.auth_token.as_deref(), Some("tok"));
        assert_eq!(cfg.github_token.as_deref(), Some("ghtok"));
        assert_eq!(cfg.editor.as_deref(), Some("nvim"));
    }

    #[test]
    fn flag_override_beats_xdg_and_home() {
        let dir = resolve_config_dir(
            Some(Path::new("/custom/cfg")),
            Some("/xdg".into()),
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(dir.unwrap(), PathBuf::from("/custom/cfg"));
    }

    #[test]
    fn xdg_config_home_beats_home() {
        let dir = resolve_config_dir(None, Some("/xdg".into()), Some(PathBuf::from("/home/u")));
        assert_eq!(dir.unwrap(), PathBuf::from("/xdg/lazygocd"));
    }

    #[test]
    fn empty_xdg_falls_back_to_home() {
        let dir = resolve_config_dir(None, Some("".into()), Some(PathBuf::from("/home/u")));
        assert_eq!(dir.unwrap(), PathBuf::from("/home/u/.config/lazygocd"));
    }

    #[test]
    fn no_xdg_falls_back_to_home() {
        let dir = resolve_config_dir(None, None, Some(PathBuf::from("/home/u")));
        assert_eq!(dir.unwrap(), PathBuf::from("/home/u/.config/lazygocd"));
    }
}
