use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
}

fn default_poll_interval() -> u64 {
    30
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
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config").join("lazygocd").join("config.toml"))
}

/// Local disk cache of the last successful dashboard load, so the next launch
/// can show data instantly instead of waiting on a fresh network round trip.
pub fn dashboard_cache_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config").join("lazygocd").join("dashboard_cache.json"))
}

/// Starred pipeline names, pinned to the top of the tree regardless of group.
pub fn favorites_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config").join("lazygocd").join("favorites.json"))
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
        toml::from_str(&text).with_context(|| format!("parsing config file at {}", path.display()))?
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

    Ok(cfg)
}

pub fn save(path: &PathBuf, cfg: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(path, text).with_context(|| format!("writing config to {}", path.display()))?;

    // Contains a password/token in plaintext: restrict to owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }

    Ok(())
}
