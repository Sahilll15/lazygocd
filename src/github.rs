use anyhow::{Context, Result};
use crate::api::encode_segment;
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

/// What a successful check found, plus whether the configured token had to be
/// abandoned to get it. The UI says so rather than switching credentials silently.
pub struct CommitCheck {
    pub sha: String,
    pub fell_back: bool,
}

/// One HTTP attempt. Transport failures are the outer Err; an HTTP rejection is
/// data, because a 401 or 403 is worth a second attempt with another token.
enum Attempt {
    Ok(String),
    Rejected { status: u16, body: String },
}

pub fn gh_auth_token() -> Option<String> {
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

/// Token precedence: explicit config/env token, then whatever `gh auth token`
/// yields for users who already have the GitHub CLI signed in - no PAT pasting.
pub fn resolve_token(configured: Option<String>) -> Option<String> {
    if configured.as_deref().is_some_and(|t| !t.trim().is_empty()) {
        return configured;
    }
    gh_auth_token()
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

    #[cfg(test)]
    pub fn api_base_for_test(&self) -> &str {
        &self.api_base
    }

    fn attempt(&self, url: &str, token: Option<&str>) -> Result<Attempt> {
        let mut rb = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "lazygocd");
        // The GitHub token belongs to a different trust domain than GoCD, so it
        // never goes out over a connection nothing authenticates.
        if let Some(token) = token
            && self.api_base.starts_with("https://")
        {
            rb = rb.bearer_auth(token);
        }
        let resp = rb.send().context("requesting latest commit")?;
        let status = resp.status();
        let body = resp.text().context("reading GitHub response body")?;
        if !status.is_success() {
            return Ok(Attempt::Rejected {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: CommitResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing GitHub commit response: {}", truncate(&body)))?;
        Ok(Attempt::Ok(parsed.sha))
    }

    /// Latest commit SHA on a branch. Works unauthenticated for public repos
    /// (rate-limited); private repos need a token with repo read access.
    ///
    /// A rejected token is retried once against `gh auth token`. A hand-made
    /// classic PAT is not SSO-authorized for an org, while the gh CLI's OAuth
    /// token usually is, so the configured token can fail where gh succeeds.
    pub fn latest_commit(&self, owner: &str, repo: &str, branch: &str) -> Result<CommitCheck> {
        let url = format!(
            "{}/repos/{}/{}/commits/{}",
            self.api_base,
            encode_segment(owner),
            encode_segment(repo),
            encode_segment(branch)
        );
        let what = format!("{owner}/{repo}@{branch}");

        let first = self.attempt(&url, self.token.as_deref())?;
        let (status, body) = match first {
            Attempt::Ok(sha) => {
                return Ok(CommitCheck {
                    sha,
                    fell_back: false,
                });
            }
            Attempt::Rejected { status, body } => (status, body),
        };

        if matches!(status, 401 | 403) {
            let fallback = gh_auth_token().filter(|t| Some(t) != self.token.as_ref());
            if let Some(fallback) = fallback
                && let Ok(Attempt::Ok(sha)) = self.attempt(&url, Some(&fallback))
            {
                return Ok(CommitCheck {
                    sha,
                    fell_back: true,
                });
            }
        }

        anyhow::bail!("{}", explain_rejection(status, &body, owner, &what))
    }

}

/// A 401 or 403 has one likely cause each, and saying which turns a dead end
/// into an action. Anything else keeps GitHub's own wording.
fn explain_rejection(status: u16, body: &str, owner: &str, what: &str) -> String {
    match status {
        403 if body.contains("SAML") || body.contains("SSO") => format!(
            "GitHub 403 for {what}: this token is not SSO-authorized for the {owner} \
             organization. Authorize it under Settings > Developer settings > Personal \
             access tokens > Configure SSO, or clear github_token to use `gh auth token`."
        ),
        401 => format!("GitHub 401 for {what}: the token is invalid or revoked."),
        403 => format!(
            "GitHub 403 for {what}: the token lacks access, or you are rate limited. {}",
            truncate(body)
        ),
        404 => format!(
            "GitHub 404 for {what}: no such repo or branch, or the token cannot see it."
        ),
        _ => format!("GitHub returned {status} for {what}: {}", truncate(body)),
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    // An explicitly configured token must win, and a blank one must not count
    // as configured: that would silently skip the `gh auth token` fallback.
    #[test]
    fn configured_token_wins_and_blank_is_not_configured() {
        assert_eq!(super::resolve_token(Some("ghp_explicit".into())), Some("ghp_explicit".into()));
        // Blank or whitespace falls through to the gh CLI (which may return
        // None on a machine without gh; either way it is not the blank value).
        assert_ne!(super::resolve_token(Some("   ".into())), Some("   ".into()));
        assert_ne!(super::resolve_token(Some("".into())), Some("".into()));
    }

    // A SAML-blocked token is the case that used to render as "connect GitHub
    // with '@'", telling people to do the thing they had already done.
    #[test]
    fn saml_rejection_names_the_org_and_both_fixes() {
        let body = r#"{"message":"Resource protected by organization SAML enforcement. You must grant your Personal Access token access to this organization."}"#;
        let out = super::explain_rejection(403, body, "acme", "acme/web-app@main");
        assert!(out.contains("SSO-authorized"), "{out}");
        assert!(out.contains("acme"), "names the org: {out}");
        assert!(out.contains("github_token"), "names the config escape hatch: {out}");
    }

    #[test]
    fn other_rejections_say_what_they_are() {
        assert!(super::explain_rejection(401, "{}", "acme", "x").contains("invalid or revoked"));
        assert!(super::explain_rejection(404, "{}", "acme", "x").contains("no such repo"));
        // A plain 403 with no SAML marker must not claim an SSO problem.
        let plain = super::explain_rejection(403, r#"{"message":"rate limit"}"#, "acme", "x");
        assert!(!plain.contains("SSO-authorized"), "{plain}");
        assert!(plain.contains("rate limited"), "{plain}");
    }

    #[test]
    fn api_base_trailing_slash_is_normalised() {
        // A trailing slash would produce '//repos/...' in every request URL.
        let c = super::GitHubClient::new(None, "https://ghe.corp.io/api/v3/").unwrap();
        assert_eq!(c.api_base_for_test(), "https://ghe.corp.io/api/v3");
        let d = super::GitHubClient::new(None, "https://api.github.com").unwrap();
        assert_eq!(d.api_base_for_test(), "https://api.github.com");
    }
}
