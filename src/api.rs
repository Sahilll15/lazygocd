use crate::config::Config;
use crate::model::{ArtifactNode, DashboardEmbedded, DashboardResponse, HistoryResponse, PipelineInstance};
use anyhow::{Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::Method;

#[derive(Clone)]
pub struct GoCdClient {
    client: Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    token: Option<String>,
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
            token: cfg.auth_token.clone(),
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
    pub fn fetch_dashboard(&self, etag: Option<&str>) -> Result<Option<(DashboardEmbedded, Option<String>)>> {
        let mut rb = self.request(Method::GET, "/api/dashboard", 4);
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
        let mut path = format!("/api/pipelines/{pipeline_name}/history");
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
            anyhow::bail!("GoCD returned {status} for {pipeline_name} history: {}", truncate(&body));
        }
        let parsed: HistoryResponse = serde_json::from_str(&body)
            .with_context(|| format!("parsing history response for {pipeline_name}: {}", truncate(&body)))?;
        let next = parsed
            .links
            .and_then(|l| l.next)
            .and_then(|n| crate::model::next_page_cursor(&n.href));
        Ok((parsed.pipelines, next))
    }

    pub fn trigger_pipeline(&self, pipeline_name: &str) -> Result<()> {
        let path = format!("/api/pipelines/{pipeline_name}/schedule");
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
            anyhow::bail!("GoCD returned {status} triggering {pipeline_name}: {}", truncate(&body));
        }
        Ok(())
    }

    /// Trigger with one-off environment variable overrides for this run.
    pub fn trigger_pipeline_with_vars(&self, pipeline_name: &str, vars: &[(String, String)]) -> Result<()> {
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
            anyhow::bail!("GoCD returned {status} triggering {pipeline_name}: {}", truncate(&body));
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
        self.rerun(pipeline_name, pipeline_counter, stage_name, stage_counter, "run-failed-jobs")
    }

    /// Reruns the whole stage instance (all jobs, passed ones included).
    pub fn rerun_stage(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
    ) -> Result<()> {
        self.rerun(pipeline_name, pipeline_counter, stage_name, stage_counter, "run")
    }

    fn rerun(
        &self,
        pipeline_name: &str,
        pipeline_counter: i64,
        stage_name: &str,
        stage_counter: &str,
        verb: &str,
    ) -> Result<()> {
        let path = format!("/api/stages/{pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}/{verb}");
        let resp = self
            .request(Method::POST, &path, 3)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| format!("rerunning ({verb}) {pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GoCD returned {status} rerunning stage: {}", truncate(&body));
        }
        Ok(())
    }

    pub fn pause_pipeline(&self, pipeline_name: &str, cause: &str) -> Result<()> {
        let path = format!("/api/pipelines/{pipeline_name}/pause");
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .json(&serde_json::json!({ "pause_cause": cause }))
            .send()
            .with_context(|| format!("pausing {pipeline_name}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GoCD returned {status} pausing {pipeline_name}: {}", truncate(&body));
        }
        Ok(())
    }

    pub fn unpause_pipeline(&self, pipeline_name: &str) -> Result<()> {
        let path = format!("/api/pipelines/{pipeline_name}/unpause");
        let resp = self
            .request(Method::POST, &path, 1)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| format!("unpausing {pipeline_name}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GoCD returned {status} unpausing {pipeline_name}: {}", truncate(&body));
        }
        Ok(())
    }

    /// Cancels a currently-running stage instance. Does not affect future
    /// scheduling (that's pause/unpause) - this stops a build in flight.
    pub fn cancel_stage(&self, pipeline_name: &str, pipeline_counter: i64, stage_name: &str, stage_counter: &str) -> Result<()> {
        let path = format!("/api/stages/{pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}/cancel");
        let resp = self
            .request(Method::POST, &path, 3)
            .header("X-GoCD-Confirm", "true")
            .send()
            .with_context(|| format!("cancelling {pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("GoCD returned {status} cancelling stage: {}", truncate(&body));
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
        let path = format!("/files/{pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}/{job_name}.json");
        let resp = self.request_raw(Method::GET, &path).send().context("requesting artifacts")?;
        let status = resp.status();
        let body = resp.text().context("reading artifacts body")?;
        if !status.is_success() {
            anyhow::bail!("GoCD returned {status} for artifacts: {}", truncate(&body));
        }
        serde_json::from_str(&body).with_context(|| format!("parsing artifacts: {}", truncate(&body)))
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
        let mut path = format!(
            "/files/{pipeline_name}/{pipeline_counter}/{stage_name}/{stage_counter}/{job_name}/cruise-output/console.log"
        );
        if start_line > 0 {
            path.push_str(&format!("?startLineNumber={start_line}"));
        }
        let resp = self.request_raw(Method::GET, &path).send().context("requesting console log")?;
        let status = resp.status();
        let body = resp.text().context("reading console log body")?;
        if !status.is_success() {
            anyhow::bail!("GoCD returned {status} for console log: {}", truncate(&body));
        }
        Ok(body)
    }
}

fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}
