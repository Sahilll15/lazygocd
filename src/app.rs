use crate::api::GoCdClient;
use crate::config::Config;
use crate::github::GitHubClient;
use crate::model::{
    ArtifactNode, DashboardGroup, DashboardPipeline, GitRef, PipelineInstance, ViewFilter,
    flatten_artifacts,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::{ListState, TableState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Groups,
    History,
    Detail,
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    Trigger(String),
    TriggerWithVars {
        pipeline: String,
        vars: Vec<(String, String)>,
    },
    Pause(String),
    Unpause(String),
    CancelStage {
        pipeline: String,
        pipeline_counter: i64,
        stage: String,
        stage_counter: String,
    },
    RerunFailedJobs {
        pipeline: String,
        pipeline_counter: i64,
        stage: String,
        stage_counter: String,
    },
    RerunStage {
        pipeline: String,
        pipeline_counter: i64,
        stage: String,
        stage_counter: String,
    },
}

impl PendingAction {
    fn name(&self) -> &str {
        match self {
            PendingAction::Trigger(n) | PendingAction::Pause(n) | PendingAction::Unpause(n) => n,
            PendingAction::TriggerWithVars { pipeline, .. }
            | PendingAction::CancelStage { pipeline, .. }
            | PendingAction::RerunFailedJobs { pipeline, .. }
            | PendingAction::RerunStage { pipeline, .. } => pipeline,
        }
    }
}

/// Addresses one job instance - everything cancel and console-log need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRef {
    pub pipeline: String,
    pub pipeline_counter: i64,
    pub stage: String,
    pub stage_counter: String,
    pub job: String,
}

#[derive(Debug, Clone, Copy)]
pub enum DetailRow {
    Stage(usize),
    Job(usize, usize),
}

#[derive(Debug, Clone)]
pub struct ConsoleLogState {
    pub job_ref: JobRef,
    pub title: String,
    /// Job result at open time ("Passed"/"Failed"/...), shown colored in the status row.
    pub result: Option<String>,
    pub tab: JobTab,
    pub lines: Vec<String>,
    pub scroll: usize,
    /// Auto-follow the tail like `tail -f`; turned off by scrolling up, back on by jumping to end.
    pub following: bool,
    pub loading: bool,
    pub error: Option<String>,

    pub search: String,
    pub search_active: bool,
    pub matches: Vec<usize>,
    pub match_idx: usize,

    /// Raw tree from the API, kept so folders can be expanded on demand.
    pub artifact_tree: Option<Vec<crate::model::ArtifactNode>>,
    /// Currently visible rows, derived from the tree and the open set.
    pub artifacts: Option<Vec<crate::model::ArtifactRow>>,
    /// Paths of open folders. Empty means everything is collapsed, which is
    /// the default: an artifact tree can be deep.
    pub artifacts_expanded: std::collections::HashSet<String>,
    pub artifacts_loading: bool,
    pub artifact_selected: usize,
    /// Pre-rendered material lines captured from the run at open time.
    pub materials: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobTab {
    Console,
    Artifacts,
    Materials,
}

impl ConsoleLogState {
    pub fn recompute_matches(&mut self) {
        self.matches.clear();
        if self.search.is_empty() {
            return;
        }
        let q = self.search.to_lowercase();
        for (i, l) in self.lines.iter().enumerate() {
            if l.to_lowercase().contains(&q) {
                self.matches.push(i);
            }
        }
        self.match_idx = self.match_idx.min(self.matches.len().saturating_sub(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReauthMode {
    Connect,
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReauthStep {
    ServerUrl,
    ChooseAuthMethod,
    Username,
    Secret,
    Insecure,
}

impl ReauthStep {
    /// Steps rendered as a selectable list (arrow-key choice) rather than free text.
    pub fn is_choice(self) -> bool {
        matches!(self, ReauthStep::ChooseAuthMethod | ReauthStep::Insecure)
    }
}

#[derive(Debug, Clone)]
pub struct ReauthForm {
    pub mode: ReauthMode,
    pub step: ReauthStep,
    pub use_token: bool,
    pub server_url: String,
    pub username: String,
    pub secret: String,
    pub insecure: bool,
    pub choice_index: usize,
    pub input: String,
}

impl ReauthForm {
    fn new(mode: ReauthMode, current_server_url: &str) -> Self {
        ReauthForm {
            mode,
            step: ReauthStep::ServerUrl,
            use_token: false,
            server_url: String::new(),
            username: String::new(),
            secret: String::new(),
            insecure: false,
            choice_index: 0,
            input: current_server_url.to_string(),
        }
    }
}

/// 'T' trigger-with-variables form: NAME=VALUE entries typed one per line,
/// an empty entry moves to the confirm step.
#[derive(Debug, Clone)]
pub struct TriggerVarsForm {
    pub pipeline: String,
    pub vars: Vec<(String, String)>,
    pub input: String,
    pub confirming: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Modal {
    Help,
    Confirm {
        action: PendingAction,
        message: String,
    },
    Reauth(ReauthForm),
    GithubConnect {
        input: String,
    },
    TriggerVars(TriggerVarsForm),
    ConsoleLog(Box<ConsoleLogState>),
    /// Personalized-view picker; index 0 = "All pipelines", then one per view.
    ViewPicker {
        selected: usize,
    },
    /// Name input for saving the current fuzzy-filter matches as a new view.
    SaveView {
        input: String,
        pipelines: Vec<String>,
    },
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Failed's message isn't shown (kept soft/non-alarming in the UI); useful in Debug output.
pub enum GithubState {
    Idle,
    Checking,
    Found(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Row {
    FavoritesHeader,
    FavoritePipeline(String),
    Group {
        idx: usize,
    },
    Pipeline {
        group_idx: usize,
        pipeline_idx: usize,
    },
}

/// Name-based identity of a tree row, so the cursor survives dashboard
/// refreshes that reorder or add/remove groups and pipelines.
enum SelectionKey {
    FavoritesHeader,
    Favorite(String),
    Group(String),
    Pipeline(String),
}

/// Fresh dashboard payload: groups, pipelines, and the response's ETag.
pub type DashboardPayload = (Vec<DashboardGroup>, Vec<DashboardPipeline>, Option<String>);

/// One history page: its runs plus the next-page cursor, if more exist.
pub type HistoryPage = (Vec<PipelineInstance>, Option<u64>);

pub enum ApiEvent {
    /// None payload = HTTP 304, nothing changed since the ETag we sent.
    Dashboard(u64, Result<Option<DashboardPayload>, String>),
    /// First page (full reload) of a pipeline's history.
    History(String, Result<HistoryPage, String>),
    /// A later page to append. `issued_last_counter` is the counter of the last
    /// loaded run when the fetch started - if it moved (a refresh interleaved),
    /// the page is stale and dropped.
    HistoryMore {
        name: String,
        after: u64,
        issued_last_counter: i64,
        result: Result<HistoryPage, String>,
    },
    ActionDone(PendingAction, Result<String, String>),
    /// (pipeline, GitRef::key() of the material this check was for, result).
    GithubLatest(String, String, Result<String, String>),
    /// usize = the 0-based line this fetch started from (0 = full log).
    ConsoleLog(JobRef, usize, Result<String, String>),
    Artifacts(JobRef, Result<Vec<ArtifactNode>, String>),
    Views(u64, Result<Vec<ViewFilter>, String>),
    /// Git refs traced through upstream pipeline dependencies (deploy pipelines
    /// have no Git material of their own).
    UpstreamRefs(String, Vec<GitRef>),
    ViewSaved(u64, Result<String, String>),
}

pub struct App {
    pub client: GoCdClient,
    pub cfg: Config,
    pub server_url: String,
    pub tx: Sender<ApiEvent>,
    pub rx: Receiver<ApiEvent>,

    pub groups: Vec<DashboardGroup>,
    pub expanded: HashSet<String>,
    pub pipeline_info: HashMap<String, DashboardPipeline>,

    /// Personalized dashboard views from the server; active_view indexes into
    /// it and filters the whole tree.
    pub views: Vec<ViewFilter>,
    pub active_view: Option<usize>,
    /// Set when the views fetch failed. Absence of views and failure to load
    /// them are different states and must never share a message.
    pub views_error: Option<String>,

    /// Starred pipeline names, pinned to a section at the top of the tree.
    pub favorites: HashSet<String>,
    pub favorites_expanded: bool,

    pub filter: String,
    pub filter_active: bool,

    pub rows: Vec<Row>,
    pub selected: usize,
    pub focus: Focus,

    /// Persistent widget states: keeps scroll offsets stable across frames and
    /// lets mouse hit-testing map a clicked y to the right row.
    pub tree_state: ListState,
    pub history_state: TableState,
    pub tree_area: Rect,
    pub history_area: Rect,
    pub detail_area: Rect,
    /// Console log content height from the last draw; scroll-up clamps against
    /// it first, since following mode parks `scroll` at usize::MAX.
    pub console_view_height: u16,

    pub selected_pipeline: Option<String>,
    pub history: Vec<PipelineInstance>,
    pub history_selected: usize,
    pub history_loading: bool,
    /// Every pipeline's last-fetched history, so reopening one already viewed
    /// this session is instant while a background refresh keeps it current.
    pub history_cache: HashMap<String, Vec<PipelineInstance>>,
    /// Next-page cursor per pipeline; absent = fully loaded (or never fetched).
    pub history_next: HashMap<String, u64>,
    /// (pipeline, after-cursor) of the one in-flight page append, if any.
    pub history_more_inflight: Option<(String, u64)>,
    /// Debounced hover target: (pipeline name, when the cursor landed on it).
    /// Prefetched into history_cache if the cursor stays put past HOVER_PREFETCH_DELAY.
    pub hover_target: Option<(String, Instant)>,
    pub last_poll: Instant,

    /// Flattened stage/job rows for the currently-selected history entry, navigable
    /// when focus is Detail.
    pub detail_rows: Vec<DetailRow>,
    pub detail_selected: usize,
    pub last_console_poll: Instant,

    pub github: GitHubClient,
    /// One check per git material of the open pipeline's latest run, in material
    /// order. Entry 0 is the "primary" material that drives 'o' compare links.
    pub github_checks: Vec<(GitRef, GithubState)>,
    /// Mirror of the FIRST material's check, kept for the compare-URL logic.
    pub github_state: GithubState,

    /// `--demo`: fixtures only. Nothing is read from or written to the real
    /// config directory, so a demo can be screen-shared safely.
    pub demo: bool,

    /// Set by 'e'. Only the main loop can suspend the terminal safely, so it
    /// drains this rather than the app spawning an editor mid-draw.
    pub pending_edit: Option<crate::editor::EditRequest>,

    /// Bumped on reconnect; in-flight responses from a previous server are
    /// dropped when their generation doesn't match.
    pub server_gen: u64,
    /// Last dashboard ETag; sent as If-None-Match so unchanged polls cost 304+0 bytes.
    pub dashboard_etag: Option<String>,

    pub loading_groups: bool,
    pub status_line: String,
    pub error_line: Option<String>,
    pub modal: Option<Modal>,
    pub should_quit: bool,
    /// Incremented once per drawn frame; drives spinner animation frames.
    pub tick: u64,
}

#[derive(Serialize, Deserialize)]
struct DashboardCache {
    saved_at_ms: i64,
    groups: Vec<DashboardGroup>,
    pipelines: Vec<DashboardPipeline>,
}

fn load_dashboard_cache() -> Option<DashboardCache> {
    let path = crate::config::dashboard_cache_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn load_favorites() -> HashSet<String> {
    (|| -> Option<HashSet<String>> {
        let path = crate::config::favorites_path().ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let names: Vec<String> = serde_json::from_str(&text).ok()?;
        Some(names.into_iter().collect())
    })()
    .unwrap_or_default()
}

fn save_favorites(favorites: HashSet<String>) {
    thread::spawn(move || {
        let Ok(path) = crate::config::favorites_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = crate::config::ensure_private_dir(dir);
        }
        let mut names: Vec<&String> = favorites.iter().collect();
        names.sort();
        if let Ok(text) = serde_json::to_string(&names) {
            let _ = crate::config::write_private(&path, &text);
        }
    });
}

fn save_dashboard_cache(groups: Vec<DashboardGroup>, pipelines: Vec<DashboardPipeline>) {
    thread::spawn(move || {
        let Ok(path) = crate::config::dashboard_cache_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = crate::config::ensure_private_dir(dir);
        }
        let saved_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let cache = DashboardCache {
            saved_at_ms,
            groups,
            pipelines,
        };
        if let Ok(text) = serde_json::to_string(&cache) {
            let _ = crate::config::write_private(&path, &text);
        }
    });
}

const HOVER_PREFETCH_DELAY: Duration = Duration::from_millis(300);

impl App {
    /// `--demo`: fixture-backed client, no config, no disk cache, no setup form.
    pub fn demo() -> anyhow::Result<Self> {
        let cfg = Config {
            server_url: "demo".to_string(),
            ..Config::default()
        };
        let mut app = Self::build(&cfg, GoCdClient::demo()?, true)?;
        app.server_url = "demo mode - fictional data, nothing is real".to_string();
        app.status_line = "Demo mode: no server, no credentials. Press ? for keys, q to quit.".to_string();
        Ok(app)
    }

    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let client = GoCdClient::new(cfg)?;
        Self::build(cfg, client, false)
    }

    fn build(cfg: &Config, client: GoCdClient, demo: bool) -> anyhow::Result<Self> {
        let github = GitHubClient::new(cfg.github_token.clone(), &cfg.github_api_base)?;
        let (tx, rx) = mpsc::channel();
        let needs_setup = cfg.server_url.trim().is_empty();

        let cached = (!demo && !needs_setup).then(load_dashboard_cache).flatten();
        let (groups, pipeline_info, status_line) = match cached {
            Some(c) => {
                let age = format_age(c.saved_at_ms);
                let info = c
                    .pipelines
                    .into_iter()
                    .map(|p| (p.name.clone(), p))
                    .collect();
                (
                    c.groups,
                    info,
                    format!("Showing cached data ({age}), refreshing..."),
                )
            }
            None => (
                Vec::new(),
                HashMap::new(),
                if needs_setup {
                    "Connect to a GoCD server to get started".to_string()
                } else {
                    "Loading dashboard...".to_string()
                },
            ),
        };
        let has_cache = !groups.is_empty();

        let mut app = App {
            client,
            cfg: cfg.clone(),
            server_url: cfg.server_url.clone(),
            tx,
            rx,
            groups,
            // Start fully collapsed: a large org can have hundreds of groups/thousands
            // of pipelines, so eagerly expanding everything would be unusable. Use `/` to filter.
            expanded: HashSet::new(),
            pipeline_info,
            views: Vec::new(),
            active_view: None,
            views_error: None,
            demo,
            pending_edit: None,
            favorites: if demo {
                // Real favorites are internal pipeline names. Reading them here
                // leaked them into a mode built for public screenshots.
                ["web-app-deploy-prod", "api-build-test"]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect()
            } else {
                load_favorites()
            },
            favorites_expanded: true,
            filter: String::new(),
            filter_active: false,
            rows: Vec::new(),
            selected: 0,
            focus: Focus::Groups,
            tree_state: ListState::default(),
            history_state: TableState::default(),
            tree_area: Rect::default(),
            history_area: Rect::default(),
            detail_area: Rect::default(),
            console_view_height: 0,
            selected_pipeline: None,
            history: Vec::new(),
            history_selected: 0,
            history_loading: false,
            history_cache: HashMap::new(),
            history_next: HashMap::new(),
            history_more_inflight: None,
            hover_target: None,
            last_poll: Instant::now(),
            detail_rows: Vec::new(),
            detail_selected: 0,
            last_console_poll: Instant::now(),
            github,
            github_checks: Vec::new(),
            github_state: GithubState::Idle,
            server_gen: 0,
            dashboard_etag: None,
            loading_groups: !needs_setup && !has_cache,
            status_line,
            error_line: None,
            modal: needs_setup.then(|| Modal::Reauth(ReauthForm::new(ReauthMode::Connect, ""))),
            should_quit: false,
            tick: 0,
        };
        if has_cache {
            app.rebuild_rows();
        }
        if !needs_setup {
            app.spawn_load_dashboard();
        }
        Ok(app)
    }

    // ---- background dispatch ----

    fn spawn_load_views(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let generation = self.server_gen;
        thread::spawn(move || {
            let result = client
                .fetch_views()
                .map(|(v, _)| v.filters)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::Views(generation, result));
        });
    }

    fn spawn_load_dashboard(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let generation = self.server_gen;
        let etag = self.dashboard_etag.clone();
        let view = self
            .active_view
            .and_then(|i| self.views.get(i))
            .map(|v| v.name.clone());
        thread::spawn(move || {
            let result = client
                .fetch_dashboard(etag.as_deref(), view.as_deref())
                .map(|opt| opt.map(|(d, tag)| (d.pipeline_groups, d.pipelines, tag)))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::Dashboard(generation, result));
        });
    }

    fn spawn_load_history(&mut self, name: String) {
        self.history_loading = true;
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client
                .fetch_history_page(&name, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::History(name, result));
        });
    }

    /// Same fetch as spawn_load_history, but for a pipeline the cursor is merely
    /// hovering over - doesn't touch history_loading, since it isn't "open" yet.
    fn spawn_prefetch_history(&self, name: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client
                .fetch_history_page(&name, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::History(name, result));
        });
    }

    /// Selection landed on the last loaded history row: if a next-page cursor
    /// exists and nothing is already in flight, fetch and append the next page.
    fn maybe_load_more_history(&mut self) {
        if self.history_more_inflight.is_some() || self.history.is_empty() {
            return;
        }
        if self.history_selected + 1 < self.history.len() {
            return;
        }
        let Some(name) = self.selected_pipeline.clone() else {
            return;
        };
        let Some(&after) = self.history_next.get(&name) else {
            return;
        };
        let issued_last_counter = self.history.last().map(|i| i.counter).unwrap_or(0);
        self.history_more_inflight = Some((name.clone(), after));
        self.status_line = format!("\u{2026}loading more runs of {name}");
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client
                .fetch_history_page(&name, Some(after))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::HistoryMore {
                name,
                after,
                issued_last_counter,
                result,
            });
        });
    }

    /// One check per git material, each reporting back under its GitRef::key().
    fn spawn_check_github(&self, pipeline_name: String, refs: &[GitRef]) {
        for git_ref in refs {
            let github = self.github.clone();
            let tx = self.tx.clone();
            let pipeline = pipeline_name.clone();
            let key = git_ref.key();
            let (owner, repo, branch) = (
                git_ref.owner.clone(),
                git_ref.repo.clone(),
                git_ref.branch.clone(),
            );
            thread::spawn(move || {
                let result = github
                    .latest_commit(&owner, &repo, &branch)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(ApiEvent::GithubLatest(pipeline, key, result));
            });
        }
    }

    /// Resets the per-material checks for the open pipeline's latest run and
    /// kicks off one background check per material.
    fn start_github_checks(&mut self, pipeline: String) {
        let refs = self
            .history
            .first()
            .map(|i| i.git_refs())
            .unwrap_or_default();
        if refs.is_empty() {
            self.github_checks.clear();
            // A deploy pipeline's only material is usually an upstream
            // dependency, so the commit is one or more hops away.
            let deps = self
                .history
                .first()
                .map(crate::model::upstream_deps)
                .unwrap_or_default();
            if deps.is_empty() {
                self.github_state = GithubState::Idle;
            } else {
                self.github_state = GithubState::Checking;
                self.spawn_trace_upstream(pipeline, deps);
            }
            return;
        }
        self.github_checks = refs
            .iter()
            .cloned()
            .map(|r| (r, GithubState::Checking))
            .collect();
        self.github_state = GithubState::Checking;
        self.spawn_check_github(pipeline, &refs);
    }

    /// Walks upstream pipeline dependencies until a run with Git materials is
    /// found. Depth-capped: a chain can be long, and each hop is one request.
    fn spawn_trace_upstream(&self, pipeline: String, deps: Vec<(String, i64)>) {
        const MAX_HOPS: usize = 4;
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let mut queue = deps;
            let mut seen: Vec<(String, i64)> = Vec::new();
            let mut found: Vec<GitRef> = Vec::new();

            for _ in 0..MAX_HOPS {
                let Some((name, counter)) = queue.pop() else {
                    break;
                };
                if seen.contains(&(name.clone(), counter)) {
                    continue;
                }
                seen.push((name.clone(), counter));

                let Ok(inst) = client.fetch_pipeline_instance(&name, counter) else {
                    continue;
                };
                let refs = inst.git_refs();
                if !refs.is_empty() {
                    found = refs
                        .into_iter()
                        .map(|mut r| {
                            r.via = Some((name.clone(), counter));
                            r
                        })
                        .collect();
                    break;
                }
                queue.extend(crate::model::upstream_deps(&inst));
            }
            let _ = tx.send(ApiEvent::UpstreamRefs(pipeline, found));
        });
    }

    fn spawn_console_fetch(&self, job_ref: JobRef) {
        self.spawn_console_fetch_from(job_ref, 0);
    }

    fn spawn_console_fetch_from(&self, job_ref: JobRef, start_line: usize) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client
                .fetch_console_log(
                    &job_ref.pipeline,
                    job_ref.pipeline_counter,
                    &job_ref.stage,
                    &job_ref.stage_counter,
                    &job_ref.job,
                    start_line,
                )
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::ConsoleLog(job_ref, start_line, result));
        });
    }

    fn spawn_action(&self, action: PendingAction) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = match &action {
                PendingAction::Trigger(name) => client
                    .trigger_pipeline(name)
                    .map(|_| format!("Triggered {name}")),
                PendingAction::TriggerWithVars { pipeline, vars } => client
                    .trigger_pipeline_with_vars(pipeline, vars)
                    .map(|_| format!("Triggered {pipeline} with {} variable(s)", vars.len())),
                PendingAction::Pause(name) => client
                    .pause_pipeline(name, "paused via lazygocd")
                    .map(|_| format!("Paused {name}")),
                PendingAction::Unpause(name) => client
                    .unpause_pipeline(name)
                    .map(|_| format!("Unpaused {name}")),
                PendingAction::CancelStage {
                    pipeline,
                    pipeline_counter,
                    stage,
                    stage_counter,
                } => client
                    .cancel_stage(pipeline, *pipeline_counter, stage, stage_counter)
                    .map(|_| format!("Cancelled {stage} on {pipeline}")),
                PendingAction::RerunFailedJobs {
                    pipeline,
                    pipeline_counter,
                    stage,
                    stage_counter,
                } => client
                    .rerun_failed_jobs(pipeline, *pipeline_counter, stage, stage_counter)
                    .map(|_| format!("Rerunning failed jobs of {stage} on {pipeline}")),
                PendingAction::RerunStage {
                    pipeline,
                    pipeline_counter,
                    stage,
                    stage_counter,
                } => client
                    .rerun_stage(pipeline, *pipeline_counter, stage, stage_counter)
                    .map(|_| format!("Rerunning stage {stage} on {pipeline}")),
            };
            let _ = tx.send(ApiEvent::ActionDone(
                action,
                result.map_err(|e| format!("{e:#}")),
            ));
        });
    }

    // ---- event handling ----

    pub fn handle_api_event(&mut self, ev: ApiEvent) {
        match ev {
            ApiEvent::Dashboard(generation, _) if generation != self.server_gen => {
                // Stale response from before a reconnect - a different server's data.
            }
            ApiEvent::Dashboard(_, Ok(None)) => {
                // 304: nothing changed server-side; keep everything as is.
                self.loading_groups = false;
            }
            ApiEvent::ViewSaved(generation, result) => {
                if generation == self.server_gen {
                    match result {
                        Ok(name) => {
                            self.status_line =
                                format!("Saved view '{name}' (visible in the GoCD web UI too)");
                            self.spawn_load_views();
                        }
                        Err(e) => self.error_line = Some(format!("Saving view failed: {e}")),
                    }
                }
            }
            ApiEvent::UpstreamRefs(pipeline, refs) => {
                // Guard on the still-open pipeline: tracing is several requests deep.
                if self.selected_pipeline.as_deref() == Some(pipeline.as_str()) {
                    if refs.is_empty() {
                        self.github_state = GithubState::Idle;
                    } else {
                        self.github_checks =
                            refs.iter().cloned().map(|r| (r, GithubState::Checking)).collect();
                        self.spawn_check_github(pipeline, &refs);
                    }
                }
            }
            ApiEvent::Views(generation, result) => {
                if generation == self.server_gen {
                    match result {
                        Ok(mut filters) => {
                            // Default is GoCD's implicit everything-view; the picker has "All".
                            filters.retain(|f| f.name != "Default");
                            self.views = filters;
                            self.views_error = None;
                        }
                        // Stays quiet until the user presses 'v'; a background
                        // fetch failing shouldn't interrupt anything.
                        Err(e) => self.views_error = Some(e),
                    }
                }
            }
            ApiEvent::Dashboard(_, Ok(Some((groups, pipelines, etag)))) => {
                if self.views.is_empty() {
                    self.spawn_load_views();
                }
                self.dashboard_etag = etag;
                self.status_line = format!(
                    "Loaded {} group(s), {} pipeline(s)",
                    groups.len(),
                    pipelines.len()
                );
                // A successful load means connectivity is back: drop any stale network
                // error banner instead of leaving it until the next keypress.
                self.error_line = None;
                if !self.demo {
                    save_dashboard_cache(groups.clone(), pipelines.clone());
                }
                // Re-anchor the cursor by name after the refresh: groups/pipelines can
                // shift position, and a bare index would land on an unrelated row.
                let key = self.selection_key();
                let new_info: HashMap<String, DashboardPipeline> =
                    pipelines.into_iter().map(|p| (p.name.clone(), p)).collect();
                if self.cfg.notifications {
                    for name in new_failures(&self.pipeline_info, &new_info, &self.favorites) {
                        crate::notify::notify("lazygocd", &format!("Pipeline {name} failed"));
                    }
                }
                self.groups = groups;
                self.pipeline_info = new_info;
                self.loading_groups = false;
                self.rebuild_rows();
                self.restore_selection(key);
            }
            ApiEvent::Dashboard(_, Err(e)) => {
                self.loading_groups = false;
                self.error_line = Some(format!("Failed to load dashboard: {e}{}", auth_hint(&e)));
            }
            ApiEvent::History(name, Ok((instances, next))) => {
                let cached = self.history_cache.remove(&name).unwrap_or_default();
                let (instances, kept_tail) = merge_history_pages(instances, cached);
                // Cache regardless of whether this pipeline is the one currently open -
                // a hover-prefetch result lands here too, ready for an instant open later.
                self.history_cache.insert(name.clone(), instances.clone());
                // When the older tail was kept, the stored deeper cursor still applies;
                // otherwise pagination resets to page one's cursor.
                if !kept_tail {
                    match next {
                        Some(cursor) => {
                            self.history_next.insert(name.clone(), cursor);
                        }
                        None => {
                            self.history_next.remove(&name);
                        }
                    }
                }
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    self.history = instances;
                    // Only snap back to the latest run if the previous selection no longer
                    // exists - a background refresh shouldn't yank you away from an older run
                    // you were deliberately looking at.
                    if self.history_selected >= self.history.len() {
                        self.history_selected = 0;
                    }
                    self.rebuild_detail_rows();
                    self.history_loading = false;
                    self.status_line = format!("Loaded history for {name}");
                    self.start_github_checks(name);
                }
            }
            ApiEvent::History(name, Err(e)) => {
                // Only surface failures for the pipeline actually open - a failed
                // hover-prefetch of something merely pointed at is not news.
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    self.history_loading = false;
                    self.error_line = Some(format!(
                        "Failed to load history for {name}: {e}{}",
                        auth_hint(&e)
                    ));
                }
            }
            ApiEvent::HistoryMore {
                name,
                after,
                issued_last_counter,
                result,
            } => {
                // Only the fetch we actually issued may land; anything else is stale.
                if self.history_more_inflight.as_ref() != Some(&(name.clone(), after)) {
                    return;
                }
                self.history_more_inflight = None;
                match result {
                    Ok((instances, next)) => {
                        // If a full refresh landed meanwhile, the cache's tail moved and
                        // appending would duplicate/interleave rows - drop the page.
                        let tail_counter = self
                            .history_cache
                            .get(&name)
                            .and_then(|v| v.last())
                            .map(|i| i.counter);
                        if tail_counter != Some(issued_last_counter) {
                            return;
                        }
                        match next {
                            Some(cursor) => {
                                self.history_next.insert(name.clone(), cursor);
                            }
                            None => {
                                self.history_next.remove(&name);
                            }
                        }
                        let count = instances.len();
                        if let Some(cached) = self.history_cache.get_mut(&name) {
                            cached.extend(instances.clone());
                        }
                        if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                            self.history.extend(instances);
                            self.status_line = format!(
                                "Loaded {count} more run(s) ({} total)",
                                self.history.len()
                            );
                        }
                    }
                    Err(e) => {
                        if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                            self.error_line = Some(format!(
                                "Failed to load more history for {name}: {e}{}",
                                auth_hint(&e)
                            ));
                        }
                    }
                }
            }
            ApiEvent::ActionDone(action, Ok(msg)) => {
                self.status_line = msg;
                match &action {
                    PendingAction::Trigger(name)
                    | PendingAction::TriggerWithVars { pipeline: name, .. } => {
                        if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                            self.spawn_load_history(name.clone());
                        }
                    }
                    PendingAction::Pause(name) => {
                        if let Some(info) = self.pipeline_info.get_mut(name) {
                            info.pause_info.paused = true;
                        }
                    }
                    PendingAction::Unpause(name) => {
                        if let Some(info) = self.pipeline_info.get_mut(name) {
                            info.pause_info.paused = false;
                        }
                    }
                    PendingAction::CancelStage { pipeline, .. }
                    | PendingAction::RerunFailedJobs { pipeline, .. }
                    | PendingAction::RerunStage { pipeline, .. } => {
                        if self.selected_pipeline.as_deref() == Some(pipeline.as_str()) {
                            self.spawn_load_history(pipeline.clone());
                        }
                    }
                }
            }
            ApiEvent::ActionDone(action, Err(e)) => {
                self.error_line = Some(format!(
                    "Action on {} failed: {e}{}",
                    action.name(),
                    auth_hint(&e)
                ));
            }
            ApiEvent::GithubLatest(name, key, result) => {
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    let state = match result {
                        Ok(sha) => GithubState::Found(sha),
                        Err(e) => GithubState::Failed(e),
                    };
                    if self
                        .github_checks
                        .first()
                        .is_some_and(|(r, _)| r.key() == key)
                    {
                        self.github_state = state.clone();
                    }
                    if let Some(entry) = self.github_checks.iter_mut().find(|(r, _)| r.key() == key)
                    {
                        entry.1 = state;
                    }
                }
            }
            ApiEvent::ConsoleLog(job_ref, start_line, result) => {
                if let Some(Modal::ConsoleLog(state)) = &mut self.modal
                    && state.job_ref == job_ref
                {
                    state.loading = false;
                    match result {
                        Ok(text) => {
                            if start_line == 0 {
                                state.lines = text.lines().map(str::to_string).collect();
                            } else if start_line == state.lines.len() {
                                state.lines.extend(text.lines().map(str::to_string));
                            } else {
                                // Line count moved since this fetch was issued (e.g. a manual
                                // full refresh landed in between) - drop the stale delta.
                                return;
                            }
                            state.error = None;
                            state.recompute_matches();
                            if state.following {
                                state.scroll = usize::MAX;
                            }
                        }
                        Err(e) => state.error = Some(e),
                    }
                }
            }
            ApiEvent::Artifacts(job_ref, result) => {
                if let Some(Modal::ConsoleLog(state)) = &mut self.modal
                    && state.job_ref == job_ref
                {
                    state.artifacts_loading = false;
                    match result {
                        Ok(nodes) => {
                            state.artifacts =
                                Some(flatten_artifacts(&nodes, &state.artifacts_expanded));
                            state.artifact_tree = Some(nodes);
                            state.artifact_selected = 0;
                        }
                        Err(e) => state.error = Some(e),
                    }
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Global escape hatch: must work even while a text input is capturing chars.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        self.error_line = None;

        if let Some(modal) = self.modal.clone() {
            self.handle_modal_key(key, modal);
            return;
        }

        if self.filter_active {
            self.handle_filter_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('/') => {
                self.filter_active = true;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Groups => Focus::History,
                    Focus::History => Focus::Detail,
                    Focus::Detail => Focus::Groups,
                };
                if self.focus == Focus::Detail {
                    self.rebuild_detail_rows();
                }
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('t') => self.request_action(PendingAction::Trigger),
            KeyCode::Char('T') => self.request_trigger_with_vars(),
            KeyCode::Char('p') => self.request_pause_toggle(),
            KeyCode::Char('X') => self.request_cancel(),
            KeyCode::Char('R') => self.request_rerun(),
            KeyCode::Char('f') => self.toggle_favorite(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('y') => self.copy_selected(),
            KeyCode::Char('V') => {
                if self.filter.is_empty() {
                    self.status_line =
                        "Filter first (/), then V saves the matches as a GoCD view".to_string();
                } else {
                    let pipelines: Vec<String> = self
                        .groups
                        .iter()
                        .flat_map(|g| g.pipelines.iter())
                        .filter(|name| fuzzy_match(name, &self.filter).is_some())
                        .cloned()
                        .collect();
                    if pipelines.is_empty() {
                        self.status_line = "No pipelines match the current filter".to_string();
                    } else {
                        self.modal = Some(Modal::SaveView {
                            input: String::new(),
                            pipelines,
                        });
                    }
                }
            }
            KeyCode::Char('v') => {
                if let Some(e) = self.views_error.clone().filter(|_| self.views.is_empty()) {
                    self.error_line = Some(format!("Could not load views: {e}"));
                } else if self.views.is_empty() {
                    self.status_line =
                        "No personalized views on this server (create them in the GoCD dashboard)"
                            .to_string();
                } else {
                    self.modal = Some(Modal::ViewPicker {
                        selected: self.active_view.map_or(0, |i| i + 1),
                    });
                }
            }
            KeyCode::Char('A') => {
                self.modal = Some(Modal::Reauth(ReauthForm::new(
                    ReauthMode::Reconnect,
                    &self.server_url,
                )))
            }
            KeyCode::Char('@') => {
                let current = self.cfg.github_token.clone().unwrap_or_default();
                self.modal = Some(Modal::GithubConnect { input: current });
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(self.half_page())
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-self.half_page())
            }
            KeyCode::PageDown => self.move_selection(self.half_page()),
            KeyCode::PageUp => self.move_selection(-self.half_page()),
            KeyCode::Char('g') => self.move_selection(-(self.focused_len() as i64)),
            KeyCode::Char('G') => self.move_selection(self.focused_len() as i64),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.activate_selected(),
            KeyCode::Left | KeyCode::Char('h') => self.collapse_selected(),
            KeyCode::Esc => self.go_back(),
            _ => {}
        }
    }

    /// Esc as "back": Detail -> History -> Groups; in Groups, clear any filter.
    fn go_back(&mut self) {
        match self.focus {
            Focus::Detail => self.focus = Focus::History,
            Focus::History => self.focus = Focus::Groups,
            Focus::Groups => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild_rows();
                }
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent, modal: Modal) {
        match modal {
            Modal::Help => {
                self.modal = None;
            }
            Modal::Confirm { action, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    let label = match &action {
                        PendingAction::Trigger(n) => format!("Triggering {n}..."),
                        PendingAction::TriggerWithVars { pipeline, .. } => {
                            format!("Triggering {pipeline}...")
                        }
                        PendingAction::Pause(n) => format!("Pausing {n}..."),
                        PendingAction::Unpause(n) => format!("Unpausing {n}..."),
                        PendingAction::CancelStage {
                            pipeline, stage, ..
                        } => format!("Cancelling {stage} on {pipeline}..."),
                        PendingAction::RerunFailedJobs {
                            pipeline, stage, ..
                        } => {
                            format!("Rerunning failed jobs of {stage} on {pipeline}...")
                        }
                        PendingAction::RerunStage {
                            pipeline, stage, ..
                        } => {
                            format!("Rerunning stage {stage} on {pipeline}...")
                        }
                    };
                    self.status_line = label;
                    self.spawn_action(action);
                }
                // Escalate a failed-jobs rerun to the whole stage.
                KeyCode::Char('a') => {
                    if let PendingAction::RerunFailedJobs {
                        pipeline,
                        pipeline_counter,
                        stage,
                        stage_counter,
                    } = action
                    {
                        self.modal = None;
                        self.status_line = format!("Rerunning stage {stage} on {pipeline}...");
                        self.spawn_action(PendingAction::RerunStage {
                            pipeline,
                            pipeline_counter,
                            stage,
                            stage_counter,
                        });
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modal = None;
                }
                _ => {}
            },
            Modal::Reauth(form) => self.handle_reauth_key(key, form),
            Modal::GithubConnect { input } => self.handle_github_connect_key(key, input),
            Modal::TriggerVars(form) => self.handle_trigger_vars_key(key, form),
            Modal::ViewPicker { selected } => self.handle_view_picker_key(key, selected),
            Modal::SaveView { input, pipelines } => {
                self.handle_save_view_key(key, input, pipelines)
            }
            Modal::ConsoleLog(state) => self.handle_console_log_key(key, *state),
        }
    }

    fn handle_trigger_vars_key(&mut self, key: KeyEvent, mut form: TriggerVarsForm) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        if form.confirming {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.modal = None;
                    self.status_line = format!("Triggering {}...", form.pipeline);
                    self.spawn_action(PendingAction::TriggerWithVars {
                        pipeline: form.pipeline,
                        vars: form.vars,
                    });
                    return;
                }
                KeyCode::Char('n') => {
                    self.modal = None;
                    return;
                }
                KeyCode::Backspace => form.confirming = false,
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Backspace => {
                    form.input.pop();
                }
                KeyCode::Enter => {
                    let entry = form.input.trim().to_string();
                    if entry.is_empty() {
                        form.confirming = true;
                        form.error = None;
                    } else {
                        match parse_env_var(&entry) {
                            Some(var) => {
                                form.vars.push(var);
                                form.input.clear();
                                form.error = None;
                            }
                            None => form.error = Some("Expected NAME=VALUE".to_string()),
                        }
                    }
                }
                KeyCode::Char(c) => {
                    form.input.push(c);
                }
                _ => {}
            }
        }
        self.modal = Some(Modal::TriggerVars(form));
    }

    fn handle_console_log_key(&mut self, key: KeyEvent, mut state: ConsoleLogState) {
        // Search input mode swallows everything except its own controls.
        if state.search_active {
            match key.code {
                KeyCode::Esc => {
                    state.search_active = false;
                    state.search.clear();
                    state.matches.clear();
                }
                KeyCode::Enter => {
                    state.search_active = false;
                    self.jump_to_match(&mut state);
                }
                KeyCode::Backspace => {
                    state.search.pop();
                    state.recompute_matches();
                }
                KeyCode::Char(c) => {
                    state.search.push(c);
                    state.recompute_matches();
                }
                _ => {}
            }
            self.modal = Some(Modal::ConsoleLog(Box::new(state)));
            return;
        }

        // Tab switching works from any tab.
        match key.code {
            KeyCode::Tab | KeyCode::Char(']') => {
                state.tab = match state.tab {
                    JobTab::Console => JobTab::Artifacts,
                    JobTab::Artifacts => JobTab::Materials,
                    JobTab::Materials => JobTab::Console,
                };
                state.scroll = 0;
                self.ensure_artifacts(&mut state);
                self.modal = Some(Modal::ConsoleLog(Box::new(state)));
                return;
            }
            KeyCode::BackTab | KeyCode::Char('[') => {
                state.tab = match state.tab {
                    JobTab::Console => JobTab::Materials,
                    JobTab::Artifacts => JobTab::Console,
                    JobTab::Materials => JobTab::Artifacts,
                };
                state.scroll = 0;
                self.ensure_artifacts(&mut state);
                self.modal = Some(Modal::ConsoleLog(Box::new(state)));
                return;
            }
            KeyCode::Char('1') if state.tab != JobTab::Console => {
                state.tab = JobTab::Console;
                state.scroll = 0;
            }
            KeyCode::Char('2') if state.tab != JobTab::Artifacts => {
                state.tab = JobTab::Artifacts;
                state.scroll = 0;
                self.ensure_artifacts(&mut state);
            }
            KeyCode::Char('3') if state.tab != JobTab::Materials => {
                state.tab = JobTab::Materials;
                state.scroll = 0;
            }
            _ => {}
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = None;
                return;
            }
            KeyCode::Char('r') => {
                state.loading = true;
                self.spawn_console_fetch(state.job_ref.clone());
                if state.tab == JobTab::Artifacts {
                    state.artifacts = None;
                    self.ensure_artifacts(&mut state);
                }
            }
            KeyCode::Char('/') if state.tab == JobTab::Console => {
                state.search_active = true;
                state.search.clear();
                state.matches.clear();
            }
            KeyCode::Char('n') if state.tab == JobTab::Console && !state.matches.is_empty() => {
                state.match_idx = (state.match_idx + 1) % state.matches.len();
                self.jump_to_match(&mut state);
            }
            KeyCode::Char('N') if state.tab == JobTab::Console && !state.matches.is_empty() => {
                state.match_idx = (state.match_idx + state.matches.len() - 1) % state.matches.len();
                self.jump_to_match(&mut state);
            }
            KeyCode::Up | KeyCode::Char('k') => match state.tab {
                JobTab::Artifacts => {
                    state.artifact_selected = state.artifact_selected.saturating_sub(1)
                }
                _ => console_scroll_up(&mut state, self.console_view_height, 1),
            },
            KeyCode::Down | KeyCode::Char('j') => match state.tab {
                JobTab::Artifacts => {
                    let max = state
                        .artifacts
                        .as_ref()
                        .map_or(0, |a| a.len().saturating_sub(1));
                    state.artifact_selected = (state.artifact_selected + 1).min(max);
                }
                _ => state.scroll = state.scroll.saturating_add(1),
            },
            KeyCode::Char('e') => {
                let j = &state.job_ref;
                let (suffix, body) = match state.tab {
                    JobTab::Materials => ("materials.txt", state.materials.join("\n")),
                    _ => ("console.log", state.lines.join("\n")),
                };
                if body.trim().is_empty() {
                    self.status_line = "Nothing to open yet".to_string();
                } else {
                    self.pending_edit = Some(crate::editor::EditRequest {
                        configured: self.cfg.editor.clone(),
                        file_name: format!(
                            "{}-{}-{}-{}-{suffix}",
                            j.pipeline, j.pipeline_counter, j.stage, j.job
                        ),
                        contents: body,
                    });
                }
            }
            KeyCode::Char('y') if state.tab == JobTab::Artifacts => {
                if let Some(row) = state
                    .artifacts
                    .as_ref()
                    .and_then(|a| a.get(state.artifact_selected))
                    .cloned()
                    && let Some(url) = row.url
                {
                    match copy_to_clipboard(&url) {
                        Ok(()) => self.status_line = format!("Copied url for {}", row.name),
                        Err(e) => self.error_line = Some(format!("Copy failed: {e}")),
                    }
                }
            }
            // Enter on a folder opens or closes it; on a file it goes to the
            // browser. 'l'/right open, 'h'/left close, matching the tree pane.
            KeyCode::Enter | KeyCode::Char('o') | KeyCode::Char('l') | KeyCode::Right
                if state.tab == JobTab::Artifacts =>
            {
                if let Some(row) = state
                    .artifacts
                    .as_ref()
                    .and_then(|a| a.get(state.artifact_selected))
                    .cloned()
                {
                    if row.is_folder {
                        let closing = state.artifacts_expanded.contains(&row.path);
                        // 'l'/right only ever open, so repeated presses don't toggle shut.
                        let open_only = matches!(key.code, KeyCode::Char('l') | KeyCode::Right);
                        if closing && !open_only {
                            state.artifacts_expanded.remove(&row.path);
                        } else {
                            state.artifacts_expanded.insert(row.path.clone());
                        }
                        reflow_artifacts(&mut state);
                    } else if let Some(url) = row.url {
                        match open_url(&url) {
                            Ok(()) => self.status_line = format!("Opened {}", row.name),
                            Err(e) => self.error_line = Some(format!("Couldn't open browser: {e}")),
                        }
                    }
                }
            }
            KeyCode::Char('h') | KeyCode::Left if state.tab == JobTab::Artifacts => {
                if let Some(row) = state
                    .artifacts
                    .as_ref()
                    .and_then(|a| a.get(state.artifact_selected))
                    .cloned()
                {
                    // On a closed file or folder, step out to the parent instead
                    // of doing nothing, which is what a tree is expected to do.
                    let target = if row.is_folder && state.artifacts_expanded.contains(&row.path) {
                        Some(row.path.clone())
                    } else {
                        row.path.rsplit_once('/').map(|(parent, _)| parent.to_string())
                    };
                    if let Some(t) = target {
                        state.artifacts_expanded.remove(&t);
                        reflow_artifacts(&mut state);
                        if let Some(rows) = &state.artifacts
                            && let Some(i) = rows.iter().position(|r| r.path == t)
                        {
                            state.artifact_selected = i;
                        }
                    }
                }
            }
            KeyCode::PageUp => console_scroll_up(&mut state, self.console_view_height, 20),
            KeyCode::PageDown => {
                state.scroll = state.scroll.saturating_add(20);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                console_scroll_up(&mut state, self.console_view_height, 20)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll = state.scroll.saturating_add(20);
            }
            KeyCode::Char('G') => {
                state.scroll = usize::MAX;
                state.following = true;
            }
            KeyCode::Char('g') => {
                state.scroll = 0;
                state.following = false;
            }
            _ => {}
        }
        self.modal = Some(Modal::ConsoleLog(Box::new(state)));
    }

    /// Center the viewport on the current match and stop tail-following.
    fn jump_to_match(&self, state: &mut ConsoleLogState) {
        if let Some(&line) = state.matches.get(state.match_idx) {
            state.following = false;
            state.scroll = line.saturating_sub((self.console_view_height as usize) / 2);
        }
    }

    fn ensure_artifacts(&self, state: &mut ConsoleLogState) {
        if state.tab == JobTab::Artifacts && state.artifacts.is_none() && !state.artifacts_loading {
            state.artifacts_loading = true;
            let client = self.client.clone();
            let tx = self.tx.clone();
            let j = state.job_ref.clone();
            thread::spawn(move || {
                let result = client
                    .fetch_artifacts(
                        &j.pipeline,
                        j.pipeline_counter,
                        &j.stage,
                        &j.stage_counter,
                        &j.job,
                    )
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(ApiEvent::Artifacts(j, result));
            });
        }
    }

    fn handle_reauth_key(&mut self, key: KeyEvent, mut form: ReauthForm) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }

        if form.step.is_choice() {
            match key.code {
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k')
                | KeyCode::Tab => {
                    form.choice_index = 1 - form.choice_index;
                }
                KeyCode::Enter => match form.step {
                    ReauthStep::ChooseAuthMethod => {
                        form.use_token = form.choice_index == 1;
                        form.choice_index = 0;
                        form.step = if form.use_token {
                            ReauthStep::Secret
                        } else {
                            ReauthStep::Username
                        };
                    }
                    ReauthStep::Insecure => {
                        form.insecure = form.choice_index == 1;
                        self.apply_reauth(form);
                        return;
                    }
                    _ => unreachable!("only ChooseAuthMethod/Insecure are choice steps"),
                },
                _ => {}
            }
            self.modal = Some(Modal::Reauth(form));
            return;
        }

        match key.code {
            KeyCode::Backspace => {
                form.input.pop();
            }
            KeyCode::Char(c) => {
                form.input.push(c);
            }
            KeyCode::Enter => match form.step {
                ReauthStep::ServerUrl => {
                    let url = form.input.trim().trim_end_matches('/').to_string();
                    if !url.is_empty() {
                        form.server_url = url;
                        form.input.clear();
                        form.choice_index = 0;
                        form.step = ReauthStep::ChooseAuthMethod;
                    }
                }
                ReauthStep::Username => {
                    form.username = form.input.trim().to_string();
                    form.input.clear();
                    form.choice_index = 0;
                    form.step = if form.username.is_empty() {
                        ReauthStep::Insecure
                    } else {
                        ReauthStep::Secret
                    };
                }
                ReauthStep::Secret => {
                    form.secret = form.input.clone();
                    form.input.clear();
                    form.choice_index = 0;
                    form.step = ReauthStep::Insecure;
                }
                _ => {}
            },
            _ => {}
        }
        self.modal = Some(Modal::Reauth(form));
    }

    fn apply_reauth(&mut self, form: ReauthForm) {
        let mut cfg = self.cfg.clone();
        cfg.server_url = form.server_url;
        cfg.insecure_skip_verify = form.insecure;
        if form.use_token {
            cfg.auth_token = (!form.secret.is_empty()).then_some(form.secret);
            cfg.username = None;
            cfg.password = None;
        } else if form.username.is_empty() {
            cfg.auth_token = None;
            cfg.username = None;
            cfg.password = None;
        } else {
            cfg.auth_token = None;
            cfg.username = Some(form.username);
            cfg.password = Some(form.secret);
        }

        self.modal = None;
        match GoCdClient::new(&cfg) {
            Ok(client) => {
                self.client = client;
                self.server_url = cfg.server_url.clone();
                self.cfg = cfg.clone();
                self.error_line = None;
                if !self.demo
                    && let Ok(path) = crate::config::config_path()
                    && let Err(e) = crate::config::save(&path, &cfg)
                {
                    self.error_line = Some(format!("Reconnected, but failed to save config: {e}"));
                }
                self.status_line = "Reconnecting...".to_string();
                self.server_gen += 1;
                self.dashboard_etag = None;
                self.views.clear();
                self.active_view = None;
                self.views_error = None;
                self.groups.clear();
                self.pipeline_info.clear();
                self.rows.clear();
                self.selected = 0;
                self.selected_pipeline = None;
                self.history.clear();
                self.history_selected = 0;
                self.history_cache.clear();
                self.history_next.clear();
                self.history_more_inflight = None;
                self.expanded.clear();
                self.hover_target = None;
                self.detail_rows.clear();
                self.detail_selected = 0;
                self.github_checks.clear();
                self.github_state = GithubState::Idle;
                self.loading_groups = true;
                self.spawn_load_dashboard();
            }
            Err(e) => {
                self.error_line = Some(format!("Failed to reconnect: {e}"));
            }
        }
    }

    fn handle_github_connect_key(&mut self, key: KeyEvent, mut input: String) {
        match key.code {
            KeyCode::Esc => {
                self.modal = None;
                return;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => {
                input.push(c);
            }
            KeyCode::Enter => {
                let token = input.trim().to_string();
                self.modal = None;
                self.cfg.github_token = (!token.is_empty()).then_some(token.clone());
                match GitHubClient::new(self.cfg.github_token.clone(), &self.cfg.github_api_base) {
                    Ok(client) => {
                        self.github = client;
                        self.status_line = if token.is_empty() {
                            "GitHub disconnected (checks now unauthenticated, public repos only)"
                                .to_string()
                        } else {
                            "GitHub connected".to_string()
                        };
                        if !self.demo
                            && let Ok(path) = crate::config::config_path()
                            && let Err(e) = crate::config::save(&path, &self.cfg)
                        {
                            self.error_line =
                                Some(format!("Connected, but failed to save config: {e}"));
                        }
                        // Retry the current pipeline's checks, if any, against the new token.
                        if let Some(name) = self.selected_pipeline.clone()
                            && !self.github_checks.is_empty()
                        {
                            self.start_github_checks(name);
                        }
                    }
                    Err(e) => {
                        self.error_line = Some(format!("Failed to update GitHub client: {e}"));
                    }
                }
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::GithubConnect { input });
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter_active = false;
                self.filter.clear();
                self.rebuild_rows();
            }
            KeyCode::Enter => {
                self.filter_active = false;
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_rows();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.rebuild_rows();
            }
            _ => {}
        }
    }

    fn refresh(&mut self) {
        self.loading_groups = true;
        self.status_line = "Refreshing...".to_string();
        self.spawn_load_dashboard();
        if let Some(name) = self.selected_pipeline.clone() {
            self.spawn_load_history(name);
        }
    }

    fn update_hover_target(&mut self) {
        let name = match self.rows.get(self.selected) {
            Some(Row::Pipeline {
                group_idx,
                pipeline_idx,
            }) => self
                .groups
                .get(*group_idx)
                .and_then(|g| g.pipelines.get(*pipeline_idx))
                .cloned(),
            Some(Row::FavoritePipeline(name)) => Some(name.clone()),
            _ => None,
        };
        match name {
            Some(name) => {
                let already_hovering = self.hover_target.as_ref().is_some_and(|(n, _)| *n == name);
                if !already_hovering {
                    self.hover_target = Some((name, Instant::now()));
                }
            }
            None => self.hover_target = None,
        }
    }

    /// Called every main-loop tick: if the cursor has sat on the same pipeline
    /// row past the debounce delay, start loading its history in the background.
    pub fn maybe_prefetch(&mut self) {
        let Some((name, since)) = &self.hover_target else {
            return;
        };
        if since.elapsed() < HOVER_PREFETCH_DELAY {
            return;
        }
        let name = name.clone();
        self.hover_target = None;
        if !self.history_cache.contains_key(&name) {
            self.spawn_prefetch_history(name);
        }
    }

    /// Called every main-loop tick: silently refresh the dashboard (and the
    /// open pipeline's history) once poll_interval_secs has elapsed.
    pub fn maybe_poll(&mut self) {
        if self.server_url.trim().is_empty() {
            return;
        }
        let interval = Duration::from_secs(self.cfg.poll_interval_secs.max(5));
        if self.last_poll.elapsed() < interval {
            return;
        }
        self.last_poll = Instant::now();
        self.spawn_load_dashboard();
        if let Some(name) = self.selected_pipeline.clone() {
            self.spawn_load_history(name);
        }
    }

    /// Called every main-loop tick: while a console log modal is open and its
    /// stage is still running, silently re-fetch every few seconds - stops on
    /// its own once the stage completes, so it doesn't poll a finished log forever.
    pub fn maybe_poll_console(&mut self) {
        let Some(Modal::ConsoleLog(state)) = &self.modal else {
            return;
        };
        if state.loading {
            return;
        }
        let job_ref = state.job_ref.clone();

        let still_active = self
            .history
            .iter()
            .find(|i| i.counter == job_ref.pipeline_counter)
            .and_then(|i| i.stages.iter().find(|s| s.name == job_ref.stage))
            .map(|s| s.is_active())
            .unwrap_or(false);
        if !still_active || self.last_console_poll.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.last_console_poll = Instant::now();
        let from = match &self.modal {
            Some(Modal::ConsoleLog(s)) => s.lines.len(),
            _ => 0,
        };
        self.spawn_console_fetch_from(job_ref, from);
    }

    /// True while anything on screen is animating (spinners); the main loop
    /// only redraws unprompted, and advances `tick`, while this holds.
    pub fn needs_animation(&self) -> bool {
        self.loading_groups
            || self.history_loading
            || self
                .github_checks
                .iter()
                .any(|(_, s)| matches!(s, GithubState::Checking))
            || matches!(&self.modal, Some(Modal::ConsoleLog(s)) if s.loading || s.artifacts_loading)
    }

    // ---- mouse ----

    /// Returns true if the event changed anything worth redrawing.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> bool {
        match ev.kind {
            MouseEventKind::ScrollUp => self.mouse_scroll(ev.column, ev.row, -1),
            MouseEventKind::ScrollDown => self.mouse_scroll(ev.column, ev.row, 1),
            MouseEventKind::Down(MouseButton::Left) => self.mouse_click(ev.column, ev.row),
            _ => false,
        }
    }

    fn mouse_scroll(&mut self, x: u16, y: u16, delta: i64) -> bool {
        let view_height = self.console_view_height;
        if let Some(Modal::ConsoleLog(state)) = &mut self.modal {
            if delta < 0 {
                console_scroll_up(state, view_height, 3);
            } else {
                state.scroll = state.scroll.saturating_add(3);
            }
            return true;
        }
        if self.modal.is_some() {
            return false;
        }
        let Some(pane) = self.pane_at(x, y) else {
            return false;
        };
        self.move_selection_in(pane, delta);
        true
    }

    fn mouse_click(&mut self, x: u16, y: u16) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let Some(pane) = self.pane_at(x, y) else {
            return false;
        };
        // A click while filtering commits the filter, like Enter, then selects.
        self.filter_active = false;
        let focus_changed = self.focus != pane;
        if focus_changed {
            self.focus = pane;
            if pane == Focus::Detail {
                self.rebuild_detail_rows();
            }
        }
        let Some(idx) = self.row_at(pane, y) else {
            return true;
        };
        match pane {
            Focus::Groups => {
                if !focus_changed && self.selected == idx {
                    self.activate_selected();
                } else {
                    self.selected = idx;
                    self.update_hover_target();
                }
            }
            Focus::History => {
                if self.history_selected != idx {
                    self.history_selected = idx;
                    self.rebuild_detail_rows();
                    self.maybe_load_more_history();
                }
            }
            Focus::Detail => {}
        }
        true
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<Focus> {
        let hit = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        if hit(self.tree_area) {
            Some(Focus::Groups)
        } else if hit(self.history_area) {
            Some(Focus::History)
        } else if hit(self.detail_area) {
            Some(Focus::Detail)
        } else {
            None
        }
    }

    /// Maps a clicked y to a row index, accounting for the border line (and the
    /// history table's header row) plus the widget's current scroll offset.
    fn row_at(&self, pane: Focus, y: u16) -> Option<usize> {
        let (area, first_y, offset, len) = match pane {
            Focus::Groups => (
                self.tree_area,
                self.tree_area.y + 1,
                self.tree_state.offset(),
                self.rows.len(),
            ),
            Focus::History => (
                self.history_area,
                self.history_area.y + 2,
                self.history_state.offset(),
                self.history.len(),
            ),
            Focus::Detail => return None,
        };
        let last_y = area.y + area.height.saturating_sub(1);
        if y < first_y || y >= last_y {
            return None;
        }
        let idx = offset + (y - first_y) as usize;
        (idx < len).then_some(idx)
    }

    fn request_action(&mut self, make: impl FnOnce(String) -> PendingAction) {
        if let Some(name) = self.current_row_pipeline_name() {
            let action = make(name.clone());
            let message = match &action {
                PendingAction::Trigger(_) => format!("{name}\nTrigger a new run?"),
                PendingAction::Pause(_) => format!("{name}\nPause this pipeline?"),
                PendingAction::Unpause(_) => format!("{name}\nUnpause this pipeline?"),
                // request_action is only ever called with Trigger; the other actions
                // go through their own request_* fns, which build their own messages.
                _ => unreachable!(),
            };
            self.modal = Some(Modal::Confirm { action, message });
        }
    }

    /// 'T': trigger with one-off environment variable overrides.
    fn request_trigger_with_vars(&mut self) {
        if let Some(name) = self.current_row_pipeline_name() {
            self.modal = Some(Modal::TriggerVars(TriggerVarsForm {
                pipeline: name,
                vars: Vec::new(),
                input: String::new(),
                confirming: false,
                error: None,
            }));
        }
    }

    /// 'R': rerun the failed jobs of the selected run's failed stage. In Detail
    /// focus the stage under the cursor wins if it failed; otherwise the run's
    /// first failed stage.
    fn request_rerun(&mut self) {
        if self.focus == Focus::Groups {
            self.status_line = "Open a pipeline first (enter) to rerun a failed stage".to_string();
            return;
        }
        let Some(pipeline) = self.selected_pipeline.clone() else {
            return;
        };
        let Some(inst) = self.history.get(self.history_selected) else {
            return;
        };
        if inst.overall_status() == "Passed" {
            self.status_line = format!("Run #{} passed - nothing failed to rerun", inst.counter);
            return;
        }
        let cursor_stage_idx = (self.focus == Focus::Detail)
            .then(|| self.detail_rows.get(self.detail_selected))
            .flatten()
            .map(|r| match r {
                DetailRow::Stage(si) | DetailRow::Job(si, _) => *si,
            });
        let failed = |s: &&crate::model::StageInstance| s.status.as_deref() == Some("Failed");
        let stage = cursor_stage_idx
            .and_then(|si| inst.stages.get(si))
            .filter(|s| failed(s))
            .or_else(|| inst.stages.iter().find(failed));
        let Some(stage) = stage else {
            self.status_line = format!("No failed stage in run #{}", inst.counter);
            return;
        };
        let Some(stage_counter) = stage.counter.clone() else {
            self.error_line = Some("Stage counter unavailable, can't rerun".to_string());
            return;
        };
        let stage_name = stage.name.clone();
        let message = format!(
            "{pipeline}  run #{}\nRerun the failed jobs of stage '{stage_name}'?\n('a' reruns the whole stage instead)",
            inst.counter
        );
        let action = PendingAction::RerunFailedJobs {
            pipeline,
            pipeline_counter: inst.counter,
            stage: stage_name,
            stage_counter,
        };
        self.modal = Some(Modal::Confirm { action, message });
    }

    fn request_pause_toggle(&mut self) {
        if let Some(name) = self.current_row_pipeline_name() {
            let paused = self
                .pipeline_info
                .get(&name)
                .map(|p| p.pause_info.paused)
                .unwrap_or(false);
            let action = if paused {
                PendingAction::Unpause(name.clone())
            } else {
                PendingAction::Pause(name.clone())
            };
            let message = if paused {
                format!("{name}\nUnpause this pipeline?")
            } else {
                format!("{name}\nPause this pipeline?")
            };
            self.modal = Some(Modal::Confirm { action, message });
        }
    }

    /// Cancels a stage in progress - not the same as pause/unpause, which only
    /// affects future scheduling and can't touch a build already running.
    fn request_cancel(&mut self) {
        let Some(pipeline) = self.selected_pipeline.clone() else {
            self.status_line =
                "Open a pipeline first (enter) to cancel a running build".to_string();
            return;
        };
        let Some(inst) = self.history.first() else {
            return;
        };
        let Some(stage) = inst.stages.iter().find(|s| s.is_active()) else {
            self.status_line = format!("Nothing currently running in {pipeline}");
            return;
        };
        let Some(stage_counter) = stage.counter.clone() else {
            self.error_line = Some("Stage counter unavailable, can't cancel".to_string());
            return;
        };
        let stage_name = stage.name.clone();
        let action = PendingAction::CancelStage {
            pipeline: pipeline.clone(),
            pipeline_counter: inst.counter,
            stage: stage_name.clone(),
            stage_counter,
        };
        let message = format!(
            "{pipeline}  run #{}\nCancel the running '{stage_name}' stage?",
            inst.counter
        );
        self.modal = Some(Modal::Confirm { action, message });
    }

    fn toggle_favorite(&mut self) {
        let Some(name) = self.current_row_pipeline_name() else {
            return;
        };
        if self.favorites.remove(&name) {
            self.status_line = format!("Removed {name} from favorites");
        } else {
            self.favorites.insert(name.clone());
            self.status_line = format!("Added {name} to favorites");
        }
        if !self.demo {
            save_favorites(self.favorites.clone());
        }
        self.rebuild_rows();
    }

    /// 'y': copy the most useful identifier for the current selection - the
    /// run's full commit SHA in history/details, the pipeline or group name
    /// in the tree.
    fn copy_selected(&mut self) {
        let (what, text) = match self.focus {
            Focus::History | Focus::Detail => {
                let Some(inst) = self.history.get(self.history_selected) else {
                    return;
                };
                match inst.git_modification().and_then(|m| m.revision.clone()) {
                    Some(sha) => ("commit sha", sha),
                    None => ("run label", inst.label.clone()),
                }
            }
            Focus::Groups => match self.rows.get(self.selected) {
                Some(Row::Pipeline {
                    group_idx,
                    pipeline_idx,
                }) => {
                    let Some(name) = self
                        .groups
                        .get(*group_idx)
                        .and_then(|g| g.pipelines.get(*pipeline_idx))
                    else {
                        return;
                    };
                    ("pipeline name", name.clone())
                }
                Some(Row::FavoritePipeline(name)) => ("pipeline name", name.clone()),
                Some(Row::Group { idx }) => {
                    let Some(g) = self.groups.get(*idx) else {
                        return;
                    };
                    ("group name", g.name.clone())
                }
                _ => return,
            },
        };
        match copy_to_clipboard(&text) {
            Ok(()) => self.status_line = format!("Copied {what}: {text}"),
            Err(e) => self.error_line = Some(format!("Copy failed: {e}")),
        }
    }

    /// 'o': open the relevant GitHub page for the selected run - the pending
    /// compare view when we know the latest deploy is behind the branch head,
    /// otherwise that run's commit.
    fn open_in_browser(&mut self) {
        let inst = match self.focus {
            Focus::History | Focus::Detail => self.history.get(self.history_selected),
            Focus::Groups => self
                .current_row_pipeline_name()
                .and_then(|name| self.history_cache.get(&name))
                .and_then(|runs| runs.first()),
        };
        let Some(inst) = inst else {
            self.status_line = "Open a pipeline first to jump to its commit".to_string();
            return;
        };
        let Some(git_ref) = inst.git_ref() else {
            self.status_line = "This run has no direct git material to open".to_string();
            return;
        };

        let is_latest_run = self.history_selected == 0 && self.focus != Focus::Groups;
        let url = match (&self.github_state, is_latest_run) {
            (GithubState::Found(latest), true) if *latest != git_ref.deployed_sha => format!(
                "https://{}/{}/{}/compare/{}...{}",
                git_ref.host, git_ref.owner, git_ref.repo, git_ref.deployed_sha, latest
            ),
            _ => format!(
                "https://{}/{}/{}/commit/{}",
                git_ref.host, git_ref.owner, git_ref.repo, git_ref.deployed_sha
            ),
        };
        match open_url(&url) {
            Ok(()) => self.status_line = format!("Opened {url}"),
            Err(e) => self.error_line = Some(format!("Couldn't open browser: {e}")),
        }
    }

    pub fn current_row_pipeline_name(&self) -> Option<String> {
        match self.focus {
            Focus::Groups => match self.rows.get(self.selected)? {
                Row::Pipeline {
                    group_idx,
                    pipeline_idx,
                } => self
                    .groups
                    .get(*group_idx)?
                    .pipelines
                    .get(*pipeline_idx)
                    .cloned(),
                Row::FavoritePipeline(name) => Some(name.clone()),
                Row::Group { .. } | Row::FavoritesHeader => None,
            },
            Focus::History | Focus::Detail => self.selected_pipeline.clone(),
        }
    }

    fn move_selection(&mut self, delta: i64) {
        self.move_selection_in(self.focus, delta);
    }

    fn move_selection_in(&mut self, pane: Focus, delta: i64) {
        let (current, len) = match pane {
            Focus::Groups => (self.selected, self.rows.len()),
            Focus::History => (self.history_selected, self.history.len()),
            Focus::Detail => (self.detail_selected, self.detail_rows.len()),
        };
        if len == 0 {
            return;
        }
        let idx = (current as i64 + delta).clamp(0, len as i64 - 1) as usize;
        match pane {
            Focus::Groups => {
                self.selected = idx;
                self.update_hover_target();
            }
            Focus::History => {
                self.history_selected = idx;
                self.rebuild_detail_rows();
                self.maybe_load_more_history();
            }
            Focus::Detail => self.detail_selected = idx,
        }
    }

    fn focused_len(&self) -> usize {
        match self.focus {
            Focus::Groups => self.rows.len(),
            Focus::History => self.history.len(),
            Focus::Detail => self.detail_rows.len(),
        }
    }

    /// Half the focused pane's visible rows, for ctrl-d/u and PageUp/Down jumps.
    fn half_page(&self) -> i64 {
        let height = match self.focus {
            Focus::Groups => self.tree_area.height,
            Focus::History => self.history_area.height.saturating_sub(1),
            Focus::Detail => self.detail_area.height,
        };
        i64::from((height.saturating_sub(2) / 2).max(1))
    }

    fn rebuild_detail_rows(&mut self) {
        self.detail_rows.clear();
        if let Some(inst) = self.history.get(self.history_selected) {
            for (si, stage) in inst.stages.iter().enumerate() {
                self.detail_rows.push(DetailRow::Stage(si));
                for ji in 0..stage.jobs.len() {
                    self.detail_rows.push(DetailRow::Job(si, ji));
                }
            }
        }
        if self.detail_selected >= self.detail_rows.len() {
            self.detail_selected = self.detail_rows.len().saturating_sub(1);
        }
    }

    fn activate_selected(&mut self) {
        match self.focus {
            Focus::Groups => match self.rows.get(self.selected).cloned() {
                Some(Row::Group { idx }) => {
                    if let Some(g) = self.groups.get(idx) {
                        let name = g.name.clone();
                        if self.expanded.contains(&name) {
                            self.expanded.remove(&name);
                        } else {
                            self.expanded.insert(name);
                        }
                        self.rebuild_rows();
                    }
                }
                Some(Row::Pipeline {
                    group_idx,
                    pipeline_idx,
                }) => {
                    if let Some(name) = self
                        .groups
                        .get(group_idx)
                        .and_then(|g| g.pipelines.get(pipeline_idx))
                        .cloned()
                    {
                        self.open_pipeline(name);
                    }
                }
                Some(Row::FavoritesHeader) => {
                    self.favorites_expanded = !self.favorites_expanded;
                    self.rebuild_rows();
                }
                Some(Row::FavoritePipeline(name)) => {
                    self.open_pipeline(name);
                }
                None => {}
            },
            Focus::History => {}
            Focus::Detail => {
                if let Some(DetailRow::Job(si, ji)) =
                    self.detail_rows.get(self.detail_selected).copied()
                {
                    self.open_console_log_for(si, ji);
                }
            }
        }
    }

    fn open_pipeline(&mut self, name: String) {
        self.selected_pipeline = Some(name.clone());
        self.history_selected = 0;
        self.focus = Focus::History;

        if let Some(cached) = self.history_cache.get(&name).cloned() {
            // Instant from cache; still refresh in the background to stay current.
            self.history = cached;
            self.history_loading = false;
            self.status_line = format!("{name} (cached, refreshing...)");
            self.start_github_checks(name.clone());
        } else {
            self.history.clear();
            self.github_checks.clear();
            self.github_state = GithubState::Idle;
            self.status_line = format!("Loading history for {name}...");
        }
        self.rebuild_detail_rows();
        self.detail_selected = 0;
        self.spawn_load_history(name);
    }

    fn open_console_log_for(&mut self, stage_idx: usize, job_idx: usize) {
        let Some(pipeline) = self.selected_pipeline.clone() else {
            return;
        };
        let Some(inst) = self.history.get(self.history_selected) else {
            return;
        };
        let Some(stage) = inst.stages.get(stage_idx) else {
            return;
        };
        let Some(job) = stage.jobs.get(job_idx) else {
            return;
        };
        let Some(stage_counter) = stage.counter.clone() else {
            self.error_line =
                Some("Stage counter unavailable, can't fetch console log".to_string());
            return;
        };
        let job_ref = JobRef {
            pipeline,
            pipeline_counter: inst.counter,
            stage: stage.name.clone(),
            stage_counter,
            job: job.name.clone(),
        };
        let mut materials = Vec::new();
        if let Some(cause) = &inst.build_cause {
            if let Some(msg) = &cause.trigger_message {
                materials.push(format!("Trigger: {msg}"));
            }
            for mr in &cause.material_revisions {
                if let Some(desc) = &mr.material.description {
                    materials.push(String::new());
                    materials.push(desc.clone());
                }
                for m in &mr.modifications {
                    let sha = m.revision.as_deref().unwrap_or("-");
                    let who = m.user_name.as_deref().unwrap_or("-");
                    let short: String = sha.chars().take(12).collect();
                    materials.push(format!("  {short} by {who}"));
                    if let Some(c) = &m.comment {
                        for line in c.lines().take(3) {
                            materials.push(format!("    {line}"));
                        }
                    }
                }
            }
        }
        self.modal = Some(Modal::ConsoleLog(Box::new(ConsoleLogState {
            title: format!(
                "{}/{} - {}",
                job_ref.stage, job_ref.stage_counter, job_ref.job
            ),
            job_ref: job_ref.clone(),
            result: job.result.clone().or_else(|| job.state.clone()),
            tab: JobTab::Console,
            lines: Vec::new(),
            scroll: 0,
            following: true,
            loading: true,
            error: None,
            search: String::new(),
            search_active: false,
            matches: Vec::new(),
            match_idx: 0,
            artifact_tree: None,
            artifacts: None,
            artifacts_expanded: std::collections::HashSet::new(),
            artifacts_loading: false,
            artifact_selected: 0,
            materials,
        })));
        self.last_console_poll = Instant::now();
        self.spawn_console_fetch(job_ref);
    }

    fn collapse_selected(&mut self) {
        if self.focus != Focus::Groups {
            return;
        }
        match self.rows.get(self.selected).cloned() {
            Some(Row::Group { idx }) => {
                if let Some(g) = self.groups.get(idx) {
                    self.expanded.remove(&g.name);
                    self.rebuild_rows();
                }
            }
            Some(Row::Pipeline { group_idx, .. }) => {
                if let Some(g) = self.groups.get(group_idx) {
                    let name = g.name.clone();
                    self.expanded.remove(&name);
                    self.rebuild_rows();
                    if let Some(pos) = self
                        .rows
                        .iter()
                        .position(|r| matches!(r, Row::Group{idx} if *idx == group_idx))
                    {
                        self.selected = pos;
                    }
                }
            }
            Some(Row::FavoritePipeline(_)) => {
                self.favorites_expanded = false;
                self.rebuild_rows();
                if let Some(pos) = self
                    .rows
                    .iter()
                    .position(|r| matches!(r, Row::FavoritesHeader))
                {
                    self.selected = pos;
                }
            }
            Some(Row::FavoritesHeader) | None => {}
        }
    }

    /// Name-based identity of the selected tree row, resolved against the
    /// current `groups` - capture it before swapping in fresh dashboard data.
    fn selection_key(&self) -> Option<SelectionKey> {
        match self.rows.get(self.selected)? {
            Row::FavoritesHeader => Some(SelectionKey::FavoritesHeader),
            Row::FavoritePipeline(name) => Some(SelectionKey::Favorite(name.clone())),
            Row::Group { idx } => Some(SelectionKey::Group(self.groups.get(*idx)?.name.clone())),
            Row::Pipeline {
                group_idx,
                pipeline_idx,
            } => Some(SelectionKey::Pipeline(
                self.groups
                    .get(*group_idx)?
                    .pipelines
                    .get(*pipeline_idx)?
                    .clone(),
            )),
        }
    }

    fn restore_selection(&mut self, key: Option<SelectionKey>) {
        let Some(key) = key else { return };
        let pos = self.rows.iter().position(|row| match (row, &key) {
            (Row::FavoritesHeader, SelectionKey::FavoritesHeader) => true,
            (Row::FavoritePipeline(n), SelectionKey::Favorite(k)) => n == k,
            (Row::Group { idx }, SelectionKey::Group(k)) => {
                self.groups.get(*idx).is_some_and(|g| g.name == *k)
            }
            (
                Row::Pipeline {
                    group_idx,
                    pipeline_idx,
                },
                SelectionKey::Pipeline(k),
            ) => self
                .groups
                .get(*group_idx)
                .and_then(|g| g.pipelines.get(*pipeline_idx))
                .is_some_and(|n| n == k),
            _ => false,
        });
        if let Some(pos) = pos {
            self.selected = pos;
        }
    }

    fn handle_save_view_key(&mut self, key: KeyEvent, mut input: String, pipelines: Vec<String>) {
        match key.code {
            KeyCode::Esc => {
                self.modal = None;
                return;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            KeyCode::Enter => {
                let name = input.trim().to_string();
                if name.is_empty() {
                    return;
                }
                self.modal = None;
                self.status_line =
                    format!("Saving view '{name}' ({} pipelines)...", pipelines.len());
                let client = self.client.clone();
                let tx = self.tx.clone();
                let generation = self.server_gen;
                thread::spawn(move || {
                    let result = client
                        .save_view(&name, pipelines)
                        .map(|_| name)
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(ApiEvent::ViewSaved(generation, result));
                });
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::SaveView { input, pipelines });
    }

    fn handle_view_picker_key(&mut self, key: KeyEvent, mut selected: usize) {
        let count = self.views.len() + 1; // slot 0 = All pipelines
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => {
                self.modal = None;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => selected = (selected + 1).min(count - 1),
            KeyCode::Char('g') | KeyCode::Home => selected = 0,
            KeyCode::Char('G') | KeyCode::End => selected = count - 1,
            KeyCode::Enter => {
                self.active_view = (selected > 0).then(|| selected - 1);
                self.modal = None;
                self.status_line = match self.active_view {
                    Some(i) => format!("Loading view: {}...", self.views[i].name),
                    None => "Loading all pipelines...".to_string(),
                };
                // Views are server-side filters (the bare dashboard already has the
                // user's Default view applied), so switching means refetching.
                self.dashboard_etag = None;
                self.loading_groups = true;
                self.selected = 0;
                self.spawn_load_dashboard();
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::ViewPicker { selected });
    }

    /// (paused, building, failed) across the whole fleet, for the header counts.
    pub fn fleet_counts(&self) -> (u32, u32, u32) {
        let mut paused = 0;
        let mut building = 0;
        let mut failed = 0;
        for p in self.pipeline_info.values() {
            if p.pause_info.paused {
                paused += 1;
            }
            match p.latest_status() {
                "Building" => building += 1,
                "Failed" => failed += 1,
                _ => {}
            }
        }
        (paused, building, failed)
    }

    pub fn rebuild_rows(&mut self) {
        let filter = self.filter.clone();
        self.rows.clear();

        // Pinned section, hidden while filtering so a search result set stays pure.
        if filter.is_empty() && !self.favorites.is_empty() {
            self.rows.push(Row::FavoritesHeader);
            if self.favorites_expanded {
                let mut names: Vec<&String> = self.favorites.iter().collect();
                names.sort();
                for name in names {
                    self.rows.push(Row::FavoritePipeline(name.clone()));
                }
            }
        }

        for (gi, group) in self.groups.iter().enumerate() {
            let matching: Vec<usize> = group
                .pipelines
                .iter()
                .enumerate()
                .filter(|(_, name)| filter.is_empty() || fuzzy_match(name, &filter).is_some())
                .map(|(pi, _)| pi)
                .collect();

            if !filter.is_empty() && matching.is_empty() {
                continue;
            }

            self.rows.push(Row::Group { idx: gi });
            let expanded = if filter.is_empty() {
                self.expanded.contains(&group.name)
            } else {
                true
            };
            if expanded {
                for pi in matching {
                    self.rows.push(Row::Pipeline {
                        group_idx: gi,
                        pipeline_idx: pi,
                    });
                }
            }
        }
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }
}

/// Following mode parks scroll at usize::MAX; pin it back to the real bottom
/// before scrolling up, or the subtraction never becomes visible.
/// Recomputes the visible rows after an expand or collapse, keeping the
/// selection inside the new list.
fn reflow_artifacts(state: &mut ConsoleLogState) {
    if let Some(tree) = &state.artifact_tree {
        let rows = flatten_artifacts(tree, &state.artifacts_expanded);
        state.artifact_selected = state.artifact_selected.min(rows.len().saturating_sub(1));
        state.artifacts = Some(rows);
    }
}

fn console_scroll_up(state: &mut ConsoleLogState, view_height: u16, lines: usize) {
    let content_len = match state.tab {
        JobTab::Materials => state.materials.len(),
        _ => state.lines.len(),
    };
    let max = content_len.saturating_sub(view_height as usize);
    state.scroll = state.scroll.min(max).saturating_sub(lines);
    state.following = false;
}

/// A page-one reload landed while deeper pages were already loaded (background
/// polls do this): keep the older tail when the fresh page overlaps it, so
/// pagination the user scrolled into survives refreshes. Returns the merged
/// list and whether the previously-stored next-page cursor still applies.
fn merge_history_pages(
    fresh: Vec<PipelineInstance>,
    cached: Vec<PipelineInstance>,
) -> (Vec<PipelineInstance>, bool) {
    let (Some(fresh_last), Some(cached_first)) = (fresh.last(), cached.first()) else {
        return (fresh, false);
    };
    // A fresh page that doesn't reach back into the cached range would leave a
    // gap between the two - drop the tail rather than show a hole.
    if cached.len() <= fresh.len() || fresh_last.counter > cached_first.counter {
        return (fresh, false);
    }
    let cutoff = fresh_last.counter;
    let mut merged = fresh;
    merged.extend(cached.into_iter().filter(|i| i.counter < cutoff));
    (merged, true)
}

/// "NAME=VALUE" -> (NAME, VALUE). Name is trimmed and must be nonempty; the
/// value is kept verbatim (it may itself contain '=').
fn parse_env_var(entry: &str) -> Option<(String, String)> {
    let (name, value) = entry.split_once('=')?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_string(), value.to_string()))
}

/// Favorited pipelines whose latest-run status transitioned to Failed between
/// two dashboard snapshots. Edge-triggered (old status must be known and
/// non-Failed), so a pipeline that stays red never re-notifies.
fn new_failures(
    old: &HashMap<String, DashboardPipeline>,
    new: &HashMap<String, DashboardPipeline>,
    favorites: &HashSet<String>,
) -> Vec<String> {
    let mut names: Vec<String> = favorites
        .iter()
        .filter(|name| {
            new.get(*name).map(|p| p.latest_status()) == Some("Failed")
                && old
                    .get(*name)
                    .is_some_and(|p| p.latest_status() != "Failed")
        })
        .cloned()
        .collect();
    names.sort();
    names
}

/// fzf-style subsequence match: every query char (spaces ignored) must appear
/// in order, case-insensitive. Returns the matched char positions for highlighting.
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<Vec<usize>> {
    let mut hay = haystack.chars().enumerate();
    needle
        .chars()
        .filter(|c| *c != ' ')
        .map(|nc| {
            hay.by_ref()
                .find(|(_, hc)| hc.eq_ignore_ascii_case(&nc))
                .map(|(i, _)| i)
        })
        .collect()
}

/// OSC 52 clipboard write: works in modern terminals and over SSH, no native deps.
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn open_url(url: &str) -> std::io::Result<()> {
    // Windows routes through `cmd /C start`, which re-parses &, | and ^ that
    // std's argument quoting leaves alone. The URL is built from server data.
    if !url.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-' | '_' | '~' | '?' | '=' | '#')
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to open a URL containing unexpected characters",
        ));
    }
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    // `start` is a cmd builtin; the empty quoted arg is its window-title slot.
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

fn auth_hint(e: &str) -> &'static str {
    if e.contains("401") {
        "  (press A to reconnect)"
    } else if e.contains("403") {
        "  (VPN down, or the token lost access? A reconnects)"
    } else if e.contains("timed out") || e.contains("connect") {
        "  (server unreachable; r retries)"
    } else {
        ""
    }
}

fn format_age(saved_at_ms: i64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(saved_at_ms);
    let secs = (now_ms - saved_at_ms).max(0) / 1000;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    // Windows opens URLs through `cmd /C start`, which re-parses metacharacters
    // that std's argument quoting leaves untouched.
    #[test]
    fn open_url_refuses_shell_metacharacters() {
        for hostile in [
            "https://github.com&calc/acme/web-app/commit/abc",
            "https://github.com/acme/web-app/commit/abc|whoami",
            "https://github.com/acme/web app/commit/abc",
            "https://github.com/acme/web-app/commit/`id`",
            "https://github.com/acme/%PATH%/commit/abc",
        ] {
            assert!(super::open_url(hostile).is_err(), "accepted {hostile:?}");
        }
    }

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }

    use super::fuzzy_match;

    #[test]
    fn fuzzy_match_subsequence() {
        assert_eq!(
            fuzzy_match("deploy-to-production", "dtp"),
            Some(vec![0, 7, 10])
        );
        assert_eq!(
            fuzzy_match("Deploy-Prod", "dep prod"),
            Some(vec![0, 1, 2, 7, 8, 9, 10])
        );
        assert_eq!(fuzzy_match("alpha", "px"), None);
        assert_eq!(fuzzy_match("abc", ""), Some(vec![]));
        // Chars must appear in order, not just anywhere.
        assert_eq!(fuzzy_match("ba", "ab"), None);
    }

    #[test]
    fn parse_env_var_forms() {
        assert_eq!(
            super::parse_env_var("FOO=bar"),
            Some(("FOO".into(), "bar".into()))
        );
        assert_eq!(
            super::parse_env_var(" FOO =a=b"),
            Some(("FOO".into(), "a=b".into()))
        );
        assert_eq!(
            super::parse_env_var("FOO="),
            Some(("FOO".into(), String::new()))
        );
        assert_eq!(super::parse_env_var("=bar"), None);
        assert_eq!(super::parse_env_var("no-equals"), None);
    }

    use crate::model::{
        DashboardInstance, DashboardInstanceEmbedded, DashboardPipeline, DashboardPipelineEmbedded,
        DashboardStage, PauseInfo,
    };
    use std::collections::{HashMap, HashSet};

    fn pipeline(name: &str, status: &str) -> DashboardPipeline {
        DashboardPipeline {
            name: name.to_string(),
            locked: false,
            pause_info: PauseInfo::default(),
            can_pause: true,
            can_operate: true,
            embedded: DashboardPipelineEmbedded {
                instances: vec![DashboardInstance {
                    label: "l".into(),
                    counter: 1,
                    triggered_by: None,
                    embedded: DashboardInstanceEmbedded {
                        stages: vec![DashboardStage {
                            name: "build".into(),
                            status: Some(status.to_string()),
                        }],
                    },
                }],
            },
        }
    }

    fn snapshot(entries: &[(&str, &str)]) -> HashMap<String, DashboardPipeline> {
        entries
            .iter()
            .map(|(n, s)| (n.to_string(), pipeline(n, s)))
            .collect()
    }

    #[test]
    fn new_failures_fires_only_on_favorited_edge_transitions() {
        let favorites: HashSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let old = snapshot(&[("a", "Passed"), ("b", "Failed"), ("d", "Passed")]);
        let new = snapshot(&[
            ("a", "Failed"),
            ("b", "Failed"),
            ("c", "Failed"),
            ("d", "Failed"),
        ]);
        // a: Passed->Failed and favorited -> fires. b: already Failed -> no.
        // c: unknown before (no observed transition) -> no. d: not favorited -> no.
        assert_eq!(
            super::new_failures(&old, &new, &favorites),
            vec!["a".to_string()]
        );
    }

    fn run(counter: i64) -> crate::model::PipelineInstance {
        crate::model::PipelineInstance {
            name: "p".into(),
            label: format!("l{counter}"),
            counter,
            comment: None,
            scheduled_date: None,
            stages: Vec::new(),
            build_cause: None,
        }
    }

    #[test]
    fn merge_history_pages_keeps_paginated_tail_on_overlap() {
        // Cached: 61..=46 (paginated). Fresh page one after 3 new runs: 64..=57.
        let cached: Vec<_> = (46..=61).rev().map(run).collect();
        let fresh: Vec<_> = (57..=64).rev().map(run).collect();
        let (merged, kept) = super::merge_history_pages(fresh, cached);
        assert!(kept);
        let counters: Vec<i64> = merged.iter().map(|i| i.counter).collect();
        assert_eq!(counters, (46..=64).rev().collect::<Vec<i64>>());
    }

    #[test]
    fn merge_history_pages_resets_on_gap_or_no_pagination() {
        // Gap: 10 new runs pushed the fresh page entirely above the cached range.
        let cached: Vec<_> = (54..=61).rev().map(run).collect();
        let fresh: Vec<_> = (64..=71).rev().map(run).collect();
        let (merged, kept) = super::merge_history_pages(fresh, cached);
        assert!(!kept);
        assert_eq!(merged.first().unwrap().counter, 71);
        assert_eq!(merged.len(), 8);
        // Cached no deeper than the fresh page: plain replacement.
        let (merged, kept) = super::merge_history_pages(
            (54..=61).rev().map(run).collect(),
            (54..=61).rev().map(run).collect(),
        );
        assert!(!kept);
        assert_eq!(merged.len(), 8);
        // Empty cache (first load).
        let (merged, kept) = super::merge_history_pages(vec![run(1)], Vec::new());
        assert!(!kept);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn new_failures_recovery_then_refailure_fires_again() {
        let favorites: HashSet<String> = ["a"].iter().map(|s| s.to_string()).collect();
        let red = snapshot(&[("a", "Failed")]);
        let green = snapshot(&[("a", "Passed")]);
        assert!(super::new_failures(&red, &green, &favorites).is_empty());
        assert_eq!(
            super::new_failures(&green, &red, &favorites),
            vec!["a".to_string()]
        );
    }
}
