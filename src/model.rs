use serde::{Deserialize, Serialize};

/// `/api/dashboard` (Accept v4) is the primary data source: one call returns
/// pipeline groups, membership, pause state, and latest-run status for every
/// pipeline the user can see. Verified against a real ~2,400-pipeline/large
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
    #[serde(rename = "_links", default)]
    pub links: Option<HistoryLinks>,
    #[serde(default)]
    pub pipelines: Vec<PipelineInstance>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HistoryLinks {
    #[serde(default)]
    pub next: Option<Link>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    pub href: String,
}

/// Cursor for the next history page, parsed from _links.next.href's ?after=<n>.
pub fn next_page_cursor(href: &str) -> Option<u64> {
    let query = href.split('?').nth(1)?;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("after="))
        .and_then(|v| v.parse().ok())
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

/// A git-host repo/branch/commit this pipeline instance was built from, parsed
/// from a direct Git material. Chained pipelines whose only material is an
/// upstream GoCD pipeline (not raw git) yield none - there's no single
/// commit to compare without walking the whole dependency chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRef {
    /// e.g. "github.com" or a GitHub Enterprise host; drives browser URLs.
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub deployed_sha: String,
    /// Set when the commit was traced through an upstream pipeline dependency
    /// rather than read from this run's own Git material: (pipeline, counter).
    pub via: Option<(String, i64)>,
}

impl GitRef {
    /// Identity used to match an async check result back to its material.
    pub fn key(&self) -> String {
        format!("{}/{}/{}@{}", self.host, self.owner, self.repo, self.branch)
    }
}

impl PipelineInstance {
    fn first_git_revision(&self) -> Option<&MaterialRevision> {
        self.build_cause
            .as_ref()?
            .material_revisions
            .iter()
            .find(|mr| mr.material.kind.as_deref() == Some("Git"))
    }

    /// Every direct Git material of this run, in material order.
    pub fn git_refs(&self) -> Vec<GitRef> {
        let Some(cause) = &self.build_cause else {
            return Vec::new();
        };
        cause
            .material_revisions
            .iter()
            .filter(|mr| mr.material.kind.as_deref() == Some("Git"))
            .filter_map(|mr| {
                let desc = mr.material.description.as_deref()?;
                let (host, owner, repo) = parse_git_host_owner_repo(desc)?;
                let branch = parse_branch(desc).unwrap_or_else(|| "main".to_string());
                let deployed_sha = mr.modifications.first()?.revision.clone()?;
                Some(GitRef {
                    via: None,
                    host,
                    owner,
                    repo,
                    branch,
                    deployed_sha,
                })
            })
            .collect()
    }

    pub fn git_ref(&self) -> Option<GitRef> {
        self.git_refs().into_iter().next()
    }

    /// The commit that triggered this run, if it has a direct Git material.
    pub fn git_modification(&self) -> Option<&Modification> {
        self.first_git_revision()?.modifications.first()
    }
}

/// Upstream pipeline dependencies this run was triggered by, as
/// (pipeline, counter). A deploy pipeline usually has no Git material at all:
/// its only input is a Pipeline material whose revision reads
/// "upstream-name/389/stage-name/1", so the commit lives one hop away.
pub fn upstream_deps(inst: &PipelineInstance) -> Vec<(String, i64)> {
    let Some(cause) = &inst.build_cause else {
        return Vec::new();
    };
    cause
        .material_revisions
        .iter()
        .filter(|mr| mr.material.kind.as_deref() == Some("Pipeline"))
        .filter_map(|mr| {
            let rev = mr.modifications.first()?.revision.as_deref()?;
            let mut parts = rev.split('/');
            let name = parts.next()?.to_string();
            let counter: i64 = parts.next()?.parse().ok()?;
            (!name.is_empty()).then_some((name, counter))
        })
        .collect()
}

/// GoCD's Git material description reads like:
/// "URL: git@HOST:owner/repo.git, Branch: main" or "URL: https://HOST/owner/repo, ..."
fn parse_git_host_owner_repo(desc: &str) -> Option<(String, String, String)> {
    let after = desc.split("URL: ").nth(1)?;
    let url = after.split(',').next()?.trim();
    let (host, rest) = if let Some(ssh) = url.strip_prefix("git@") {
        ssh.split_once(':')?
    } else {
        let no_scheme = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;
        no_scheme.split_once('/')?
    };
    let rest = rest
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    (!host.is_empty() && !owner.is_empty() && !repo.is_empty())
        .then(|| (host.to_string(), owner.to_string(), repo.to_string()))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRow {
    pub depth: usize,
    pub name: String,
    pub is_folder: bool,
    pub url: Option<String>,
    /// Slash-joined ancestry ("dist/sourcemaps"), the identity used to remember
    /// which folders are open. Names alone collide across branches.
    pub path: String,
    pub expanded: bool,
}

/// Rows currently visible, given the set of open folders. A build can attach a
/// deep artifact tree, so folders start closed and only opened ones are walked.
pub fn flatten_artifacts(
    nodes: &[ArtifactNode],
    expanded: &std::collections::HashSet<String>,
) -> Vec<ArtifactRow> {
    fn walk(
        nodes: &[ArtifactNode],
        depth: usize,
        prefix: &str,
        expanded: &std::collections::HashSet<String>,
        out: &mut Vec<ArtifactRow>,
    ) {
        for n in nodes {
            // GoCD marks folders with a type, but a node carrying children is
            // one regardless of what the type field says.
            let is_folder = n.kind.as_deref() == Some("folder") || !n.files.is_empty();
            let path = if prefix.is_empty() {
                n.name.clone()
            } else {
                format!("{prefix}/{}", n.name)
            };
            let open = is_folder && expanded.contains(&path);
            out.push(ArtifactRow {
                depth,
                name: n.name.clone(),
                is_folder,
                url: n.url.clone(),
                path: path.clone(),
                expanded: open,
            });
            if open {
                walk(&n.files, depth + 1, &path, expanded, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, "", expanded, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_page_cursor_parses_after_param() {
        assert_eq!(
            next_page_cursor("http://go/api/pipelines/x/history?after=205"),
            Some(205)
        );
        assert_eq!(
            next_page_cursor("/go/api/pipelines/x/history?page_size=10&after=42"),
            Some(42)
        );
        assert_eq!(
            next_page_cursor("/go/api/pipelines/x/history?after=42&page_size=10"),
            Some(42)
        );
        assert_eq!(next_page_cursor("/go/api/pipelines/x/history"), None);
        assert_eq!(next_page_cursor("/history?before=9"), None);
        assert_eq!(next_page_cursor("/history?after=notanumber"), None);
    }

    #[test]
    fn parse_host_owner_repo_forms() {
        let f = |desc: &str| parse_git_host_owner_repo(desc);
        assert_eq!(
            f("URL: git@github.com:acme/web-app.git, Branch: main"),
            Some(("github.com".into(), "acme".into(), "web-app".into()))
        );
        assert_eq!(
            f("URL: https://github.com/acme/web-app, Branch: main"),
            Some(("github.com".into(), "acme".into(), "web-app".into()))
        );
        assert_eq!(
            f("URL: git@ghe.corp.io:platform/deploy.git, Branch: release"),
            Some(("ghe.corp.io".into(), "platform".into(), "deploy".into()))
        );
        assert_eq!(
            f("URL: https://ghe.corp.io/platform/deploy.git, Branch: release"),
            Some(("ghe.corp.io".into(), "platform".into(), "deploy".into()))
        );
        assert_eq!(
            f("URL: http://git.internal/team/tool, Branch: dev"),
            Some(("git.internal".into(), "team".into(), "tool".into()))
        );
        assert_eq!(f("URL: /local/bare/repo.git, Branch: main"), None);
        assert_eq!(f("no url here"), None);
        assert_eq!(f("URL: https://host/only-owner, Branch: x"), None);
    }

    fn git_revision(url: &str, branch: &str, sha: &str) -> MaterialRevision {
        MaterialRevision {
            material: MaterialInfo {
                kind: Some("Git".into()),
                description: Some(format!("URL: {url}, Branch: {branch}")),
            },
            modifications: vec![Modification {
                revision: Some(sha.into()),
                user_name: None,
                comment: None,
                modified_time: None,
            }],
        }
    }

    #[test]
    fn git_refs_returns_all_git_materials_in_order() {
        let inst = PipelineInstance {
            name: "p".into(),
            label: "l".into(),
            counter: 1,
            comment: None,
            scheduled_date: None,
            stages: Vec::new(),
            build_cause: Some(BuildCause {
                trigger_message: None,
                approver: None,
                material_revisions: vec![
                    git_revision("git@github.com:acme/web-app.git", "main", "aaa"),
                    // Non-git materials (upstream pipelines) must be skipped, not error.
                    MaterialRevision {
                        material: MaterialInfo {
                            kind: Some("Pipeline".into()),
                            description: None,
                        },
                        modifications: Vec::new(),
                    },
                    git_revision("https://ghe.corp.io/platform/deploy", "release", "bbb"),
                ],
            }),
        };
        let refs = inst.git_refs();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            (
                refs[0].host.as_str(),
                refs[0].repo.as_str(),
                refs[0].deployed_sha.as_str()
            ),
            ("github.com", "web-app", "aaa")
        );
        assert_eq!(
            (
                refs[1].host.as_str(),
                refs[1].branch.as_str(),
                refs[1].deployed_sha.as_str()
            ),
            ("ghe.corp.io", "release", "bbb")
        );
        assert_eq!(inst.git_ref().unwrap().deployed_sha, "aaa");
        assert_eq!(refs[0].key(), "github.com/acme/web-app@main");
    }

    #[test]
    fn git_refs_empty_without_git_materials() {
        let inst = PipelineInstance {
            name: "p".into(),
            label: "l".into(),
            counter: 1,
            comment: None,
            scheduled_date: None,
            stages: Vec::new(),
            build_cause: None,
        };
        assert!(inst.git_refs().is_empty());
        assert!(inst.git_ref().is_none());
    }

    fn pipeline_material(revision: &str) -> MaterialRevision {
        MaterialRevision {
            material: MaterialInfo {
                kind: Some("Pipeline".into()),
                description: Some("upstream [ stage ]".into()),
            },
            modifications: vec![Modification {
                revision: Some(revision.into()),
                user_name: None,
                comment: None,
                modified_time: None,
            }],
        }
    }

    fn instance_with(revisions: Vec<MaterialRevision>) -> PipelineInstance {
        PipelineInstance {
            name: "deploy".into(),
            label: "l".into(),
            counter: 12,
            comment: None,
            scheduled_date: None,
            stages: Vec::new(),
            build_cause: Some(BuildCause {
                trigger_message: None,
                approver: None,
                material_revisions: revisions,
            }),
        }
    }

    // A deploy run's only material is an upstream dependency, so this parse is
    // the whole reason the GitHub check works on deploy pipelines at all.
    #[test]
    fn upstream_deps_parses_pipeline_material_revisions() {
        let inst = instance_with(vec![pipeline_material("my-build/389/lint-test-build/1")]);
        assert_eq!(upstream_deps(&inst), vec![("my-build".to_string(), 389)]);
    }

    #[test]
    fn upstream_deps_ignores_git_materials_and_malformed_revisions() {
        let git = git_revision("git@github.com:a/b.git", "main", "deadbeef");
        assert!(upstream_deps(&instance_with(vec![git])).is_empty());

        for bad in ["no-slashes", "name/notanumber/stage/1", "/389/stage/1"] {
            assert!(
                upstream_deps(&instance_with(vec![pipeline_material(bad)])).is_empty(),
                "{bad:?} should not yield a dependency"
            );
        }
    }

    #[test]
    fn upstream_deps_returns_every_dependency_in_order() {
        let inst = instance_with(vec![
            pipeline_material("build-a/10/stage/1"),
            pipeline_material("build-b/22/stage/1"),
        ]);
        assert_eq!(
            upstream_deps(&inst),
            vec![("build-a".to_string(), 10), ("build-b".to_string(), 22)]
        );
    }

    #[test]
    fn flatten_artifacts_walks_the_tree_and_keeps_depth() {
        let file = |name: &str, url: &str| ArtifactNode {
            name: name.into(),
            url: Some(url.into()),
            kind: Some("file".into()),
            files: vec![],
        };
        let tree = vec![ArtifactNode {
            name: "dist".into(),
            url: None,
            kind: Some("folder".into()),
            files: vec![
                file("app.tar.gz", "https://x/app.tar.gz"),
                ArtifactNode {
                    name: "maps".into(),
                    url: None,
                    kind: Some("folder".into()),
                    files: vec![file("a.map", "https://x/a.map")],
                },
            ],
        }];
        use std::collections::HashSet;

        // Closed by default: only the top level is visible.
        let none = HashSet::new();
        let rows = flatten_artifacts(&tree, &none);
        assert_eq!(rows.len(), 1, "a closed folder must not show its children");
        assert_eq!(rows[0].name, "dist");
        assert!(rows[0].is_folder);
        assert!(!rows[0].expanded);

        // Opening one folder reveals only its direct children.
        let open_dist: HashSet<String> = ["dist".to_string()].into_iter().collect();
        let rows = flatten_artifacts(&tree, &open_dist);
        let shape: Vec<(usize, &str, bool)> =
            rows.iter().map(|r| (r.depth, r.name.as_str(), r.is_folder)).collect();
        assert_eq!(
            shape,
            vec![(0, "dist", true), (1, "app.tar.gz", false), (1, "maps", true)],
            "a nested folder stays closed until opened itself"
        );

        // Opening the nested one too walks the full depth.
        let open_both: HashSet<String> =
            ["dist".to_string(), "dist/maps".to_string()].into_iter().collect();
        let rows = flatten_artifacts(&tree, &open_both);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].name, "a.map");
        assert_eq!(rows[3].depth, 2);
        // Paths are ancestry-qualified so same-named folders never share state.
        assert_eq!(rows[2].path, "dist/maps");

        // Files keep a URL so 'o' and 'y' have something to act on; folders don't.
        assert_eq!(rows[1].url.as_deref(), Some("https://x/app.tar.gz"));
        assert_eq!(rows[0].url, None);
    }

    // Drives whether 'X' can cancel: wrong here means offering to cancel a
    // finished run, or refusing to cancel a live one.
    #[test]
    fn stage_is_active_only_while_running_or_scheduled() {
        let stage = |st: Option<&str>| StageInstance {
            name: "build".into(),
            status: st.map(str::to_string),
            approval_type: None,
            scheduled_date: None,
            counter: None,
            jobs: vec![],
        };
        assert!(stage(Some("Building")).is_active());
        assert!(stage(Some("Scheduled")).is_active());
        for done in ["Passed", "Failed", "Cancelled", "Unknown"] {
            assert!(!stage(Some(done)).is_active(), "{done} should not be active");
        }
        assert!(!stage(None).is_active());
    }

    // A node with children is a folder even if GoCD omits the type field,
    // otherwise its children would be unreachable.
    #[test]
    fn a_node_with_children_counts_as_a_folder_without_a_type() {
        use std::collections::HashSet;
        let tree = vec![ArtifactNode {
            name: "untyped".into(),
            url: None,
            kind: None,
            files: vec![ArtifactNode {
                name: "inner.txt".into(),
                url: Some("https://x/inner.txt".into()),
                kind: None,
                files: vec![],
            }],
        }];
        let rows = flatten_artifacts(&tree, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_folder, "a node with children must be expandable");

        let open: HashSet<String> = ["untyped".to_string()].into_iter().collect();
        assert_eq!(flatten_artifacts(&tree, &open).len(), 2);
    }

    // Same folder name under different parents must not share open state.
    #[test]
    fn identically_named_folders_have_distinct_paths() {
        use std::collections::HashSet;
        let leaf = |n: &str| ArtifactNode {
            name: n.into(),
            url: Some("https://x".into()),
            kind: Some("file".into()),
            files: vec![],
        };
        let branch = |n: &str| ArtifactNode {
            name: n.into(),
            url: None,
            kind: Some("folder".into()),
            files: vec![ArtifactNode {
                name: "logs".into(),
                url: None,
                kind: Some("folder".into()),
                files: vec![leaf("a.txt")],
            }],
        };
        let tree = vec![branch("one"), branch("two")];
        let open: HashSet<String> =
            ["one".to_string(), "one/logs".to_string(), "two".to_string()].into_iter().collect();
        let rows = flatten_artifacts(&tree, &open);
        let opened: Vec<(&str, bool)> =
            rows.iter().map(|r| (r.path.as_str(), r.expanded)).collect();
        assert!(opened.contains(&("one/logs", true)));
        assert!(opened.contains(&("two/logs", false)), "two/logs must stay closed");
    }
}

/// A personalized dashboard view ("filter") from GoCD's pipeline_selection API -
/// the same custom tabs users create in the web dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFilters {
    #[serde(default)]
    pub filters: Vec<ViewFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFilter {
    pub name: String,
    /// "whitelist" (only these pipelines) or "blacklist" (all except these).
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Status filter (e.g. ["failing"]); preserved verbatim on round-trips.
    #[serde(default)]
    pub state: Vec<String>,
    #[serde(default)]
    pub pipelines: Vec<String>,
}
