use crate::config::Config;
use crate::model::{
    ArtifactNode, DashboardEmbedded, DashboardResponse, HistoryResponse, PipelineInstance,
    ViewFilters,
};
use anyhow::{Context, Result};
use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder};

#[derive(Clone)]
pub struct GoCdClient {
    client: Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    token: Option<String>,
    /// `--demo`: serve canned fixtures and never touch the network.
    demo: bool,
}

impl GoCdClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let client = Client::builder()
            .danger_accept_invalid_certs(cfg.insecure_skip_verify)
            // gzip (below) turns /api/dashboard's ~20s uncompressed transfer into ~2s;
            // keep a generous ceiling anyway for slow networks or unusually large orgs.
            .timeout(std::time::Duration::from_secs(45))
            // Separate connect deadline: a dead route (VPN drop) fails in seconds
            // instead of hanging until the full 45s response budget runs out.
            .connect_timeout(std::time::Duration::from_secs(8))
            .build()
            .context("building HTTP client")?;

        Ok(GoCdClient {
            client,
            base_url: cfg.server_url.trim_end_matches('/').to_string(),
            username: cfg.username.clone(),
            password: cfg.password.clone(),
            demo: false,
            token: cfg.auth_token.clone(),
        })
    }

    /// Demo client: no credentials, no requests, fixtures only.
    pub fn demo() -> Result<Self> {
        Ok(GoCdClient {
            client: Client::builder().build().context("building demo HTTP client")?,
            base_url: "demo".to_string(),
            username: None,
            password: None,
            token: None,
            demo: true,
        })
    }

    fn request(&self, method: Method, path: &str, api_version: u8) -> RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut rb = self.client.request(method, url).header(
            "Accept",
            format!("application/vnd.go.cd.v{api_version}+json"),
        );
        if let Some(token) = &self.token {
            rb = rb.bearer_auth(token);
        } else if let Some(user) = &self.username {
            rb = rb.basic_auth(user, self.password.clone());
        }
        rb
    }

    /// /go/files/... is a plain file server, not the versioned JSON API - no
    /// vnd.go.cd Accept header here.
    fn request_raw(&self, method: Method, path: &str) -> RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let mut rb = self.client.request(method, url);
        if let Some(token) = &self.token {
            rb = rb.bearer_auth(token);
        } else if let Some(user) = &self.username {
            rb = rb.basic_auth(user, self.password.clone());
        }
        rb
    }

    /// One call returns pipeline groups, membership, pause state, and latest-run
    /// status for every pipeline the user can see. Accept v4, verified against a
    /// real GoCD 23.5.0 instance; per-pipeline status polling doesn't scale.
    /// Pass the previous ETag to let the server answer 304 (returns Ok(None)),
    /// which skips the multi-MB payload on unchanged polls.
    pub fn fetch_dashboard(
        &self,
        etag: Option<&str>,
        view: Option<&str>,
    ) -> Result<Option<(DashboardEmbedded, Option<String>)>> {
        if self.demo {
            let parsed: DashboardResponse = serde_json::from_str(&crate::demo::dashboard_json(view))
                .context("parsing demo dashboard")?;
            return Ok(Some((parsed.embedded, Some("demo-etag".to_string()))));
        }
        let mut rb = self.request(Method::GET, "/api/dashboard", 4);
        if let Some(name) = view {
            // The server filters to the personalized view, same as the web UI's tabs.
            rb = rb.query(&[("viewName", name)]);
        }
        if let Some(tag) = etag {
            rb = rb.header("If-None-Match", tag);
        }
        let resp = rb.send().context("requesting dashboard")?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        let new_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().context("reading dashboard response body")?;
        if !status.is_success() {
            anyhow::bail!("GoCD returned {status} for dashboard: {}", truncate(&body));
        }
        let parsed: DashboardResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing dashboard response: {}", truncate(&body)))?;
        Ok(Some((parsed.embedded, new_etag)))
    }

    /// One page of run history, newest first. `after` is the cursor from the
    /// previous page's _links.next.href; None fetches the first page. Returns
    /// the page's runs plus the next-page cursor, if any.
    pub fn fetch_history_page(
        &self,
        pipeline_name: &str,
        after: Option<u64>,
    ) -> Result<(Vec<PipelineInstance>, Option<u64>)> {
        if self.demo {
            let parsed: HistoryResponse =
                serde_json::from_str(&crate::demo::history_json(pipeline_name, after))
                    .context("parsing demo history")?;
            let next = parsed
                .links
                .and_then(|l| l.next)
                .and_then(|n| crate::model::next_page_cursor(&n.href));
            return Ok((parsed.pipelines, next));
        }

        let mut path = format!("/api/pipelines/{}/history", encode_segment(pipeline_name));
        if let Some(cursor) = after {
            path.push_str(&format!("?after={cursor}"));
        }
        let resp = self
            .request(Method::GET, &path, 1)
            .send()
            .with_context(|| format!("requesting history for {pipeline_name}"))?;
        let status = resp.status();
        let body = resp.text().context("reading history response body")?;
        if !status.is_success() {
            anyhow::bail!(
                "GoCD returned {status} for {pipeline_name} history: {}",
                truncate(&body)
            );
        }
        let parsed: HistoryResponse = serde_json::from_str(&body).with_context(|| {
            format!(
                "parsing history response for {pipeline_name}: {}",
                truncate(&body)
            )
        })?;
        let next = parsed
            .links
            .and_then(|l| l.next)
            .and_then(|n| crate::model::next_page_cursor(&n.href));
        Ok((parsed.pipelines, next))
    }

    pub fn trigger_pipeline(&self, pipeline_name: &str) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let path = format!("/api/pipelines/{}/schedule", encode_segment(pipeline_name));
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .with_context(|| format!("triggering {pipeline_name}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} triggering {pipeline_name}: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    /// Trigger with one-off environment variable overrides for this run.
    pub fn trigger_pipeline_with_vars(
        &self,
        pipeline_name: &str,
        vars: &[(String, String)],
    ) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let env: Vec<serde_json::Value> = vars
            .iter()
            .map(|(name, value)| serde_json::json!({ "name": name, "value": value, "secure": false }))
            .collect();
        let path = format!("/api/pipelines/{pipeline_name}/schedule");
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .json(&serde_json::json!({
                "environment_variables": env,
                "update_materials_before_scheduling": true,
            }))
            .send()
            .with_context(|| format!("triggering {pipeline_name} with variables"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} triggering {pipeline_name}: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    /// Reruns only the failed jobs of a completed stage instance.
    pub fn rerun_failed_jobs(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
    ) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        self.rerun(
            pipeline_name,
            pipeline_counter,
            stage_name,
            stage_counter,
            "run-failed-jobs",
        )
    }

    /// Reruns the whole stage instance (all jobs, passed ones included).
    pub fn rerun_stage(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
    ) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        self.rerun(
            pipeline_name,
            pipeline_counter,
            stage_name,
            stage_counter,
            "run",
        )
    }

    fn rerun(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
        verb: &str,
    ) -> Result<()> {
        let path = format!(
            "/api/stages/{}/{pipeline_counter}/{}/{}/{verb}",
            encode_segment(pipeline_name),
            encode_segment(stage_name),
            encode_segment(stage_counter)
        );
        let resp = self
            .request(Method::POST, &path, 3)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| format!("rerunning ({verb}) {pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} rerunning stage: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    pub fn pause_pipeline(&self, pipeline_name: &str, cause: &str) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let path = format!("/api/pipelines/{}/pause", encode_segment(pipeline_name));
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .json(&serde_json::json!({ "pause_cause": cause }))
            .send()
            .with_context(|| format!("pausing {pipeline_name}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} pausing {pipeline_name}: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    pub fn unpause_pipeline(&self, pipeline_name: &str) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let path = format!("/api/pipelines/{}/unpause", encode_segment(pipeline_name));
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| format!("unpausing {pipeline_name}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} unpausing {pipeline_name}: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    /// Cancels a currently-running stage instance. Does not affect future
    /// scheduling (that's pause/unpause) - this stops a build in flight.
    pub fn cancel_stage(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
    ) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let path = format!(
            "/api/stages/{}/{pipeline_counter}/{}/{}/cancel",
            encode_segment(pipeline_name),
            encode_segment(stage_name),
            encode_segment(stage_counter)
        );
        let resp = self
            .request(Method::POST, &path, 3)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| {
                format!(
                    "cancelling {pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}"
                )
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "GoCD returned {status} cancelling stage: {}",
                truncate(&body)
            );
        }
        Ok(())
    }

    /// A single pipeline run by counter. Used to trace a deploy run's commit
    /// through its upstream pipeline dependency.
    pub fn fetch_pipeline_instance(&self, name: &str, counter: i64) -> Result<PipelineInstance> {
        if self.demo {
            let hist: HistoryResponse = serde_json::from_str(&crate::demo::history_json(name, None))
                .context("parsing demo instance")?;
            return hist
                .pipelines
                .into_iter()
                .find(|p| p.counter == counter)
                .or_else(|| {
                    serde_json::from_str::<HistoryResponse>(&crate::demo::history_json(name, None))
                        .ok()?
                        .pipelines
                        .into_iter()
                        .next()
                })
                .context("no demo instance");
        }
        let path = format!("/api/pipelines/{}/{counter}", encode_segment(name));
        let resp = self
            .request(Method::GET, &path, 1)
            .send()
            .with_context(|| format!("requesting {name} run {counter}"))?;
        let status = resp.status();
        let body = resp.text().context("reading instance body")?;
        if !status.is_success() {
            anyhow::bail!("GoCD returned {status} for {name}/{counter}: {}", truncate(&body));
        }
        serde_json::from_str(&body)
            .with_context(|| format!("parsing {name}/{counter}: {}", truncate(&body)))
    }

    /// The user's personalized dashboard views plus the optimistic-lock ETag.
    /// Internal (unversioned-contract) endpoint, but stable in practice - the
    /// web dashboard itself uses it.
    pub fn fetch_views(&self) -> Result<(ViewFilters, Option<String>)> {
        if self.demo {
            let f: ViewFilters = serde_json::from_str(crate::demo::views_json())
                .context("parsing demo views")?;
            return Ok((f, Some("demo-etag".to_string())));
        }

        let resp = self
            .request(Method::GET, "/api/internal/pipeline_selection", 1)
            .send()
            .context("requesting personalized views")?;
        let status = resp.status();
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|t| t.replace("--gzip", ""));
        let body = resp.text().context("reading views body")?;
        if !status.is_success() {
            anyhow::bail!(
                "GoCD returned {status} for pipeline_selection: {}",
                truncate(&body)
            );
        }
        let filters = serde_json::from_str(&body)
            .with_context(|| format!("parsing views: {}", truncate(&body)))?;
        Ok((filters, etag))
    }

    /// Create or update a personalized view: fetch-fresh, upsert, PUT back with
    /// If-Match so a concurrent web-UI edit fails loudly instead of being lost.
    pub fn save_view(&self, name: &str, pipelines: Vec<String>) -> Result<()> {
        if self.demo {
            return Ok(());
        }

        let (mut current, etag) = self.fetch_views()?;
        let new_filter = crate::model::ViewFilter {
            name: name.to_string(),
            kind: "whitelist".to_string(),
            state: Vec::new(),
            pipelines,
        };
        match current.filters.iter_mut().find(|f| f.name == name) {
            Some(existing) => *existing = new_filter,
            None => current.filters.push(new_filter),
        }
        let mut rb = self
            .request(Method::PUT, "/api/internal/pipeline_selection", 1)
            .header("Content-Type", "application/json")
            .json(&current);
        if let Some(tag) = etag {
            rb = rb.header("If-Match", tag);
        }
        let resp = rb.send().context("saving view")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GoCD returned {status} saving view: {}", truncate(&body));
        }
        Ok(())
    }

    /// Artifact tree for one job instance, from the plain file-server .json listing.
    pub fn fetch_artifacts(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
        job_name: &str,
    ) -> Result<Vec<ArtifactNode>> {
        if self.demo {
            return serde_json::from_str(crate::demo::artifacts_json())
                .context("parsing demo artifacts");
        }

        let path = format!(
            "/files/{}/{pipeline_counter}/{}/{}/{}.json",
            encode_segment(pipeline_name),
            encode_segment(stage_name),
            encode_segment(stage_counter),
            encode_segment(job_name)
        );
        let resp = self
            .request_raw(Method::GET, &path)
            .send()
            .context("requesting artifacts")?;
        let status = resp.status();
        let body = resp.text().context("reading artifacts body")?;
        if !status.is_success() {
            anyhow::bail!("GoCD returned {status} for artifacts: {}", truncate(&body));
        }
        serde_json::from_str(&body)
            .with_context(|| format!("parsing artifacts: {}", truncate(&body)))
    }

    /// Raw job console output. Not part of the versioned JSON API - a plain
    /// text file server endpoint, works while the job is still running too.
    /// `start_line` is 0-based; nonzero returns only lines from there on, so
    /// tail-follow appends instead of re-downloading the whole log.
    pub fn fetch_console_log(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
        job_name: &str,
        start_line: usize,
    ) -> Result<String> {
        if self.demo {
            return Ok(crate::demo::console_log(start_line));
        }

        let mut path = format!(
            "/files/{}/{pipeline_counter}/{}/{}/{}/cruise-output/console.log",
            encode_segment(pipeline_name),
            encode_segment(stage_name),
            encode_segment(stage_counter),
            encode_segment(job_name)
        );
        if start_line > 0 {
            path.push_str(&format!("?startLineNumber={start_line}"));
        }
        let resp = self
            .request_raw(Method::GET, &path)
            .send()
            .context("requesting console log")?;
        let status = resp.status();
        let body = resp.text().context("reading console log body")?;
        if !status.is_success() {
            anyhow::bail!(
                "GoCD returned {status} for console log: {}",
                truncate(&body)
            );
        }
        Ok(body)
    }
}

/// Pipeline, stage and job names come back from the server and are interpolated
/// into request paths. Encoding keeps each one inside its own path segment.
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Error bodies are normally GoCD JSON, but a proxy or load balancer in front of
/// it answers with an HTML page. Echoing that markup into a one-line status bar
/// is noise, so collapse it to something a person can act on.
fn truncate(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('<') || t.to_ascii_lowercase().contains("<html") {
        return "a proxy or gateway answered instead of GoCD".to_string();
    }
    t.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    // A proxy or load balancer in front of GoCD answers with an HTML page, and
    // that markup used to be echoed straight into the one-line status bar.
    #[test]
    fn html_error_bodies_collapse_instead_of_leaking_markup() {
        let html = "<html>\n<head><title>403 Forbidden</title></head>\n<body></body>\n</html>";
        let out = super::truncate(html);
        assert!(!out.contains('<'), "markup leaked: {out:?}");
        assert!(out.contains("proxy") || out.contains("gateway"), "unhelpful: {out:?}");

        // Leading whitespace must not defeat the detection.
        assert_eq!(super::truncate("  \n <html>x</html>"), super::truncate("<html>x</html>"));
    }

    #[test]
    fn json_error_bodies_pass_through_but_stay_bounded() {
        let json = r#"{"message":"Pipeline not found"}"#;
        assert_eq!(super::truncate(json), json);

        let long = "x".repeat(1000);
        assert_eq!(super::truncate(&long).chars().count(), 300);
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(super::encode_segment("web-app_build.1"), "web-app_build.1");
        assert_eq!(super::encode_segment("a/b"), "a%2Fb");
        assert_eq!(super::encode_segment("a?b#c"), "a%3Fb%23c");
        assert_eq!(super::encode_segment("release/1.0"), "release%2F1.0");
        assert_eq!(super::encode_segment("sp ace"), "sp%20ace");
    }

    // Non-ASCII in an error body must not panic the truncation.
    #[test]
    fn truncate_counts_characters_not_bytes() {
        let unicode = "\u{5931}\u{6557}".repeat(400);
        let out = super::truncate(&unicode);
        assert_eq!(out.chars().count(), 300);
    }
}
