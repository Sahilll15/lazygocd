use serde::{Deserialize, Serialize};

/// `/api/dashboard` (Accept v4) is the primary data source: one call returns
/// pipeline groups, membership, pause state, and latest-run status for every
/// pipeline the user can see. Verified against a real ~2,400-pipeline/185-group
/// GoCD 23.5.0 instance, where per-pipeline status polling would be infeasible.
/// Also cached to disk (see `app::save_dashboard_cache`), hence `Serialize` here too.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DashboardResponse {
    #[serde(rename = "_embedded", default)]
    pub embedded: DashboardEmbedded,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DashboardEmbedded {
    #[serde(default)]
    pub pipeline_groups: Vec<DashboardGroup>,
    #[serde(default)]
    pub pipelines: Vec<DashboardPipeline>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardGroup {
    pub name: String,
    #[serde(default)]
    pub pipelines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardPipeline {
    pub name: String,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub pause_info: PauseInfo,
    #[allow(dead_code)]
    #[serde(default)]
    pub can_pause: bool,
    #[allow(dead_code)]
    #[serde(default)]
    pub can_operate: bool,
    #[serde(rename = "_embedded", default)]
    pub embedded: DashboardPipelineEmbedded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PauseInfo {
    pub paused: bool,
    #[allow(dead_code)]
    #[serde(default)]
    pub paused_by: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub pause_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashboardPipelineEmbedded {
    #[serde(default)]
    pub instances: Vec<DashboardInstance>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardInstance {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub counter: i64,
    #[serde(default)]
    pub triggered_by: Option<String>,
    #[serde(rename = "_embedded", default)]
    pub embedded: DashboardInstanceEmbedded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DashboardInstanceEmbedded {
    #[serde(default)]
    pub stages: Vec<DashboardStage>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStage {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
}

impl DashboardPipeline {
    /// Rollup status of the most recent run, or "Unknown" if it's never run.
    pub fn latest_status(&self) -> &'static str {
        match self.embedded.instances.first() {
            Some(inst) => rollup_status(inst.embedded.stages.iter().map(|s| s.status.as_deref())),
            None => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryResponse {
    #[serde(default)]
    pub pipelines: Vec<PipelineInstance>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineInstance {
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub label: String,
    /// Numeric run counter - needed (with stage counter) to address a specific
    /// stage/job instance for cancel and console-log requests.
    #[serde(default)]
    pub counter: i64,
    #[serde(default)]
    pub comment: Option<String>,
    /// Epoch millis this run was scheduled/started.
    #[serde(default)]
    pub scheduled_date: Option<i64>,
    #[serde(default)]
    pub stages: Vec<StageInstance>,
    #[serde(default)]
    pub build_cause: Option<BuildCause>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StageInstance {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    /// "manual" or "success" (auto-triggered by the previous stage passing).
    #[serde(default)]
    pub approval_type: Option<String>,
    #[serde(default)]
    pub scheduled_date: Option<i64>,
    /// GoCD sends this as a string (e.g. "1"), not a number.
    #[serde(default)]
    pub counter: Option<String>,
    #[serde(default)]
    pub jobs: Vec<JobInstance>,
}

impl StageInstance {
    pub fn is_active(&self) -> bool {
        matches!(self.status.as_deref(), Some("Building") | Some("Scheduled"))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobInstance {
    pub name: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildCause {
    #[serde(default)]
    pub trigger_message: Option<String>,
    #[serde(default)]
    pub approver: Option<String>,
    #[serde(default)]
    pub material_revisions: Vec<MaterialRevision>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MaterialRevision {
    pub material: MaterialInfo,
    #[serde(default)]
    pub modifications: Vec<Modification>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MaterialInfo {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Modification {
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub modified_time: Option<i64>,
}

/// A GitHub repo/branch/commit this pipeline instance was built from, parsed
/// from its first direct Git material. Chained pipelines whose only material
/// is an upstream GoCD pipeline (not raw git) yield None - there's no single
/// commit to compare without walking the whole dependency chain.
#[derive(Debug, Clone)]
pub struct GitRef {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub deployed_sha: String,
}

impl PipelineInstance {
    fn first_git_revision(&self) -> Option<&MaterialRevision> {
        self.build_cause
            .as_ref()?
            .material_revisions
            .iter()
            .find(|mr| mr.material.kind.as_deref() == Some("Git"))
    }

    pub fn git_ref(&self) -> Option<GitRef> {
        let mr = self.first_git_revision()?;
        let desc = mr.material.description.as_deref()?;
        let (owner, repo) = parse_github_owner_repo(desc)?;
        let branch = parse_branch(desc).unwrap_or_else(|| "main".to_string());
        let deployed_sha = mr.modifications.first()?.revision.clone()?;
        Some(GitRef { owner, repo, branch, deployed_sha })
    }

    /// The commit that triggered this run, if it has a direct Git material.
    pub fn git_modification(&self) -> Option<&Modification> {
        self.first_git_revision()?.modifications.first()
    }
}

/// GoCD's Git material description reads like:
/// "URL: git@github.com:owner/repo.git, Branch: main"
fn parse_github_owner_repo(desc: &str) -> Option<(String, String)> {
    let after = desc.split("URL: ").nth(1)?;
    let url = after.split(',').next()?.trim();
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches(".git");
    let mut parts = rest.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    (!owner.is_empty() && !repo.is_empty()).then_some((owner, repo))
}

fn parse_branch(desc: &str) -> Option<String> {
    let idx = desc.find("Branch: ")?;
    Some(desc[idx + "Branch: ".len()..].trim().to_string())
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PipelineStatus {
    pub paused: bool,
    pub locked: bool,
    pub schedulable: bool,
}

/// Worst-case rollup across a set of stage statuses: Failed > Cancelled > Building > Passed > Unknown.
fn rollup_status<'a>(statuses: impl Iterator<Item = Option<&'a str>>) -> &'static str {
    let mut any_building = false;
    let mut any_failed = false;
    let mut any_cancelled = false;
    let mut saw_any = false;
    let mut all_passed = true;

    for status in statuses {
        saw_any = true;
        match status {
            Some("Passed") => {}
            Some("Failed") => {
                any_failed = true;
                all_passed = false;
            }
            Some("Cancelled") => {
                any_cancelled = true;
                all_passed = false;
            }
            Some("Building") | Some("Scheduled") => {
                any_building = true;
                all_passed = false;
            }
            _ => all_passed = false,
        }
    }

    if any_failed {
        "Failed"
    } else if any_cancelled {
        "Cancelled"
    } else if any_building {
        "Building"
    } else if saw_any && all_passed {
        "Passed"
    } else {
        "Unknown"
    }
}

impl PipelineInstance {
    pub fn overall_status(&self) -> &'static str {
        rollup_status(self.stages.iter().map(|s| s.status.as_deref()))
    }
}

/// One entry from the /files/:pipeline/:counter/:stage/:counter/:job.json
/// artifact listing (recursive folder tree).
#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactNode {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub files: Vec<ArtifactNode>,
}

/// Flattened artifact row for list rendering: (indent depth, name, is_folder, url).
pub type ArtifactRow = (usize, String, bool, Option<String>);

pub fn flatten_artifacts(nodes: &[ArtifactNode]) -> Vec<ArtifactRow> {
    fn walk(nodes: &[ArtifactNode], depth: usize, out: &mut Vec<ArtifactRow>) {
        for n in nodes {
            let folder = n.kind.as_deref() == Some("folder");
            out.push((depth, n.name.clone(), folder, n.url.clone()));
            walk(&n.files, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, &mut out);
    out
}
