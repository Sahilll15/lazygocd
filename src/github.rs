use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    token: Option<String>,
    /// API root, e.g. "https://api.github.com" or a GHE instance's ".../api/v3".
    api_base: String,
}

#[derive(Deserialize)]
struct CommitResponse {
    sha: String,
}

/// Token precedence: explicit config/env token, then whatever `gh auth token`
/// yields for users who already have the GitHub CLI signed in - no PAT pasting.
pub fn resolve_token(configured: Option<String>) -> Option<String> {
    if configured.as_deref().is_some_and(|t| !t.trim().is_empty()) {
        return configured;
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

impl GitHubClient {
    pub fn new(token: Option<String>, api_base: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(8))
            .build()
            .context("building GitHub HTTP client")?;
        Ok(GitHubClient {
            client,
            token: resolve_token(token),
            api_base: api_base.trim_end_matches('/').to_string(),
        })
    }

    /// Latest commit SHA on a branch. Works unauthenticated for public repos
    /// (rate-limited); private repos need a token with repo read access.
    pub fn latest_commit(&self, owner: &str, repo: &str, branch: &str) -> Result<String> {
        let url = format!("{}/repos/{owner}/{repo}/commits/{branch}", self.api_base);
        let mut rb = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "lazygocd");
        if let Some(token) = &self.token {
            rb = rb.bearer_auth(token);
        }
        let resp = rb
            .send()
            .with_context(|| format!("requesting latest commit for {owner}/{repo}@{branch}"))?;
        let status = resp.status();
        let body = resp.text().context("reading GitHub response body")?;
        if !status.is_success() {
            anyhow::bail!(
                "GitHub returned {status} for {owner}/{repo}@{branch}: {}",
                truncate(&body)
            );
        }
        let parsed: CommitResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing GitHub commit response: {}", truncate(&body)))?;
        Ok(parsed.sha)
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}
