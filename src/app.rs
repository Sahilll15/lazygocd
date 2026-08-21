use crate::api::GoCdClient;
use crate::config::Config;
use crate::github::GitHubClient;
use crate::model::{ArtifactNode, DashboardGroup, DashboardPipeline, GitRef, PipelineInstance, flatten_artifacts};
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
    Pause(String),
    Unpause(String),
    CancelStage { pipeline: String, pipeline_counter: i64, stage: String, stage_counter: String },
}

impl PendingAction {
    fn name(&self) -> &str {
        match self {
            PendingAction::Trigger(n) | PendingAction::Pause(n) | PendingAction::Unpause(n) => n,
            PendingAction::CancelStage { pipeline, .. } => pipeline,
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

    pub artifacts: Option<Vec<crate::model::ArtifactRow>>,
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

#[derive(Debug, Clone)]
pub enum Modal {
    Help,
    Confirm { action: PendingAction, message: String },
    Reauth(ReauthForm),
    GithubConnect { input: String },
    ConsoleLog(Box<ConsoleLogState>),
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
    Group { idx: usize },
    Pipeline { group_idx: usize, pipeline_idx: usize },
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

pub enum ApiEvent {
    /// None payload = HTTP 304, nothing changed since the ETag we sent.
    Dashboard(u64, Result<Option<DashboardPayload>, String>),
    History(String, Result<Vec<PipelineInstance>, String>),
    ActionDone(PendingAction, Result<String, String>),
    GithubLatest(String, Result<String, String>),
    /// usize = the 0-based line this fetch started from (0 = full log).
    ConsoleLog(JobRef, usize, Result<String, String>),
    Artifacts(JobRef, Result<Vec<ArtifactNode>, String>),
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
    pub github_ref: Option<GitRef>,
    pub github_state: GithubState,

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
        let Ok(path) = crate::config::favorites_path() else { return };
        let mut names: Vec<&String> = favorites.iter().collect();
        names.sort();
        if let Ok(text) = serde_json::to_string(&names) {
            let _ = std::fs::write(path, text);
        }
    });
}

fn save_dashboard_cache(groups: Vec<DashboardGroup>, pipelines: Vec<DashboardPipeline>) {
    thread::spawn(move || {
        let Ok(path) = crate::config::dashboard_cache_path() else { return };
        let saved_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let cache = DashboardCache { saved_at_ms, groups, pipelines };
        if let Ok(text) = serde_json::to_string(&cache) {
            let _ = std::fs::write(path, text);
        }
    });
}

const HOVER_PREFETCH_DELAY: Duration = Duration::from_millis(300);

impl App {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let client = GoCdClient::new(cfg)?;
        let github = GitHubClient::new(cfg.github_token.clone())?;
        let (tx, rx) = mpsc::channel();
        let needs_setup = cfg.server_url.trim().is_empty();

        let cached = (!needs_setup).then(load_dashboard_cache).flatten();
        let (groups, pipeline_info, status_line) = match cached {
            Some(c) => {
                let age = format_age(c.saved_at_ms);
                let info = c.pipelines.into_iter().map(|p| (p.name.clone(), p)).collect();
                (c.groups, info, format!("Showing cached data ({age}), refreshing..."))
            }
            None => (
                Vec::new(),
                HashMap::new(),
                if needs_setup { "Connect to a GoCD server to get started".to_string() } else { "Loading dashboard...".to_string() },
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
            favorites: load_favorites(),
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
            hover_target: None,
            last_poll: Instant::now(),
            detail_rows: Vec::new(),
            detail_selected: 0,
            last_console_poll: Instant::now(),
            github,
            github_ref: None,
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

    fn spawn_load_dashboard(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let generation = self.server_gen;
        let etag = self.dashboard_etag.clone();
        thread::spawn(move || {
            let result = client
                .fetch_dashboard(etag.as_deref())
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
            let result = client.fetch_history(&name).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::History(name, result));
        });
    }

    /// Same fetch as spawn_load_history, but for a pipeline the cursor is merely
    /// hovering over - doesn't touch history_loading, since it isn't "open" yet.
    fn spawn_prefetch_history(&self, name: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = client.fetch_history(&name).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::History(name, result));
        });
    }

    fn spawn_check_github(&self, pipeline_name: String, git_ref: &GitRef) {
        let github = self.github.clone();
        let tx = self.tx.clone();
        let (owner, repo, branch) = (git_ref.owner.clone(), git_ref.repo.clone(), git_ref.branch.clone());
        thread::spawn(move || {
            let result = github.latest_commit(&owner, &repo, &branch).map_err(|e| format!("{e:#}"));
            let _ = tx.send(ApiEvent::GithubLatest(pipeline_name, result));
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
                PendingAction::Trigger(name) => client.trigger_pipeline(name).map(|_| format!("Triggered {name}")),
                PendingAction::Pause(name) => client
                    .pause_pipeline(name, "paused via lazygocd")
                    .map(|_| format!("Paused {name}")),
                PendingAction::Unpause(name) => client.unpause_pipeline(name).map(|_| format!("Unpaused {name}")),
                PendingAction::CancelStage { pipeline, pipeline_counter, stage, stage_counter } => client
                    .cancel_stage(pipeline, *pipeline_counter, stage, stage_counter)
                    .map(|_| format!("Cancelled {stage} on {pipeline}")),
            };
            let _ = tx.send(ApiEvent::ActionDone(action, result.map_err(|e| format!("{e:#}"))));
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
            ApiEvent::Dashboard(_, Ok(Some((groups, pipelines, etag)))) => {
                self.dashboard_etag = etag;
                self.status_line = format!("Loaded {} group(s), {} pipeline(s)", groups.len(), pipelines.len());
                // A successful load means connectivity is back: drop any stale network
                // error banner instead of leaving it until the next keypress.
                self.error_line = None;
                save_dashboard_cache(groups.clone(), pipelines.clone());
                // Re-anchor the cursor by name after the refresh: groups/pipelines can
                // shift position, and a bare index would land on an unrelated row.
                let key = self.selection_key();
                self.groups = groups;
                self.pipeline_info = pipelines.into_iter().map(|p| (p.name.clone(), p)).collect();
                self.loading_groups = false;
                self.rebuild_rows();
                self.restore_selection(key);
            }
            ApiEvent::Dashboard(_, Err(e)) => {
                self.loading_groups = false;
                self.error_line = Some(format!("Failed to load dashboard: {e}{}", auth_hint(&e)));
            }
            ApiEvent::History(name, Ok(instances)) => {
                // Cache regardless of whether this pipeline is the one currently open -
                // a hover-prefetch result lands here too, ready for an instant open later.
                self.history_cache.insert(name.clone(), instances.clone());
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    self.github_ref = instances.first().and_then(|i| i.git_ref());
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
                    if let Some(git_ref) = self.github_ref.clone() {
                        self.github_state = GithubState::Checking;
                        self.spawn_check_github(name, &git_ref);
                    } else {
                        self.github_state = GithubState::Idle;
                    }
                }
            }
            ApiEvent::History(name, Err(e)) => {
                // Only surface failures for the pipeline actually open - a failed
                // hover-prefetch of something merely pointed at is not news.
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    self.history_loading = false;
                    self.error_line = Some(format!("Failed to load history for {name}: {e}{}", auth_hint(&e)));
                }
            }
            ApiEvent::ActionDone(action, Ok(msg)) => {
                self.status_line = msg;
                match &action {
                    PendingAction::Trigger(name) => {
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
                    PendingAction::CancelStage { pipeline, .. } => {
                        if self.selected_pipeline.as_deref() == Some(pipeline.as_str()) {
                            self.spawn_load_history(pipeline.clone());
                        }
                    }
                }
            }
            ApiEvent::ActionDone(action, Err(e)) => {
                self.error_line = Some(format!("Action on {} failed: {e}{}", action.name(), auth_hint(&e)));
            }
            ApiEvent::GithubLatest(name, result) => {
                if self.selected_pipeline.as_deref() == Some(name.as_str()) {
                    self.github_state = match result {
                        Ok(sha) => GithubState::Found(sha),
                        Err(e) => GithubState::Failed(e),
                    };
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
                            state.artifacts = Some(flatten_artifacts(&nodes));
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
            KeyCode::Char('p') => self.request_pause_toggle(),
            KeyCode::Char('X') => self.request_cancel(),
            KeyCode::Char('f') => self.toggle_favorite(),
            KeyCode::Char('o') => self.open_in_browser(),
            KeyCode::Char('y') => self.copy_selected(),
            KeyCode::Char('A') => {
                self.modal = Some(Modal::Reauth(ReauthForm::new(ReauthMode::Reconnect, &self.server_url)))
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
                        PendingAction::Pause(n) => format!("Pausing {n}..."),
                        PendingAction::Unpause(n) => format!("Unpausing {n}..."),
                        PendingAction::CancelStage { pipeline, stage, .. } => format!("Cancelling {stage} on {pipeline}..."),
                    };
                    self.status_line = label;
                    self.spawn_action(action);
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modal = None;
                }
                _ => {}
            },
            Modal::Reauth(form) => self.handle_reauth_key(key, form),
            Modal::GithubConnect { input } => self.handle_github_connect_key(key, input),
            Modal::ConsoleLog(state) => self.handle_console_log_key(key, *state),
        }
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
                JobTab::Artifacts => state.artifact_selected = state.artifact_selected.saturating_sub(1),
                _ => console_scroll_up(&mut state, self.console_view_height, 1),
            },
            KeyCode::Down | KeyCode::Char('j') => match state.tab {
                JobTab::Artifacts => {
                    let max = state.artifacts.as_ref().map_or(0, |a| a.len().saturating_sub(1));
                    state.artifact_selected = (state.artifact_selected + 1).min(max);
                }
                _ => state.scroll = state.scroll.saturating_add(1),
            },
            KeyCode::Char('y') if state.tab == JobTab::Artifacts => {
                if let Some((_, name, _, Some(url))) =
                    state.artifacts.as_ref().and_then(|a| a.get(state.artifact_selected)).cloned()
                {
                    match copy_to_clipboard(&url) {
                        Ok(()) => self.status_line = format!("Copied url for {name}"),
                        Err(e) => self.error_line = Some(format!("Copy failed: {e}")),
                    }
                }
            }
            KeyCode::Enter | KeyCode::Char('o') if state.tab == JobTab::Artifacts => {
                if let Some((_, name, folder, Some(url))) =
                    state.artifacts.as_ref().and_then(|a| a.get(state.artifact_selected)).cloned()
                    && !folder
                {
                    match open_url(&url) {
                        Ok(()) => self.status_line = format!("Opened {name}"),
                        Err(e) => self.error_line = Some(format!("Couldn't open browser: {e}")),
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
                    .fetch_artifacts(&j.pipeline, j.pipeline_counter, &j.stage, &j.stage_counter, &j.job)
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
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Tab => {
                    form.choice_index = 1 - form.choice_index;
                }
                KeyCode::Enter => match form.step {
                    ReauthStep::ChooseAuthMethod => {
                        form.use_token = form.choice_index == 1;
                        form.choice_index = 0;
                        form.step = if form.use_token { ReauthStep::Secret } else { ReauthStep::Username };
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
                    form.step = if form.username.is_empty() { ReauthStep::Insecure } else { ReauthStep::Secret };
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
                if let Ok(path) = crate::config::config_path()
                    && let Err(e) = crate::config::save(&path, &cfg)
                {
                    self.error_line = Some(format!("Reconnected, but failed to save config: {e}"));
                }
                self.status_line = "Reconnecting...".to_string();
                self.server_gen += 1;
                self.dashboard_etag = None;
                self.groups.clear();
                self.pipeline_info.clear();
                self.rows.clear();
                self.selected = 0;
                self.selected_pipeline = None;
                self.history.clear();
                self.history_selected = 0;
                self.history_cache.clear();
                self.expanded.clear();
                self.hover_target = None;
                self.detail_rows.clear();
                self.detail_selected = 0;
                self.github_ref = None;
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
                match GitHubClient::new(self.cfg.github_token.clone()) {
                    Ok(client) => {
                        self.github = client;
                        self.status_line = if token.is_empty() {
                            "GitHub disconnected (checks now unauthenticated, public repos only)".to_string()
                        } else {
                            "GitHub connected".to_string()
                        };
                        if let Ok(path) = crate::config::config_path()
                            && let Err(e) = crate::config::save(&path, &self.cfg)
                        {
                            self.error_line = Some(format!("Connected, but failed to save config: {e}"));
                        }
                        // Retry the current pipeline's check, if any, against the new token.
                        if let Some(git_ref) = self.github_ref.clone()
                            && let Some(name) = self.selected_pipeline.clone()
                        {
                            self.github_state = GithubState::Checking;
                            self.spawn_check_github(name, &git_ref);
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
            Some(Row::Pipeline { group_idx, pipeline_idx }) => {
                self.groups.get(*group_idx).and_then(|g| g.pipelines.get(*pipeline_idx)).cloned()
            }
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
        let Some((name, since)) = &self.hover_target else { return };
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
        let Some(Modal::ConsoleLog(state)) = &self.modal else { return };
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
            || matches!(self.github_state, GithubState::Checking)
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
        let Some(pane) = self.pane_at(x, y) else { return false };
        self.move_selection_in(pane, delta);
        true
    }

    fn mouse_click(&mut self, x: u16, y: u16) -> bool {
        if self.modal.is_some() {
            return false;
        }
        let Some(pane) = self.pane_at(x, y) else { return false };
        // A click while filtering commits the filter, like Enter, then selects.
        self.filter_active = false;
        let focus_changed = self.focus != pane;
        if focus_changed {
            self.focus = pane;
            if pane == Focus::Detail {
                self.rebuild_detail_rows();
            }
        }
        let Some(idx) = self.row_at(pane, y) else { return true };
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
            Focus::Groups => (self.tree_area, self.tree_area.y + 1, self.tree_state.offset(), self.rows.len()),
            Focus::History => {
                (self.history_area, self.history_area.y + 2, self.history_state.offset(), self.history.len())
            }
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
                PendingAction::Trigger(_) => format!("Trigger a new run of '{name}'?"),
                PendingAction::Pause(_) => format!("Pause '{name}'?"),
                PendingAction::Unpause(_) => format!("Unpause '{name}'?"),
                // request_action is only ever called with Trigger; CancelStage goes
                // through request_cancel(), which builds its own message.
                PendingAction::CancelStage { .. } => unreachable!(),
            };
            self.modal = Some(Modal::Confirm { action, message });
        }
    }

    fn request_pause_toggle(&mut self) {
        if let Some(name) = self.current_row_pipeline_name() {
            let paused = self.pipeline_info.get(&name).map(|p| p.pause_info.paused).unwrap_or(false);
            let action = if paused {
                PendingAction::Unpause(name.clone())
            } else {
                PendingAction::Pause(name.clone())
            };
            let message = if paused {
                format!("Unpause '{name}'?")
            } else {
                format!("Pause '{name}'?")
            };
            self.modal = Some(Modal::Confirm { action, message });
        }
    }

    /// Cancels a stage in progress - not the same as pause/unpause, which only
    /// affects future scheduling and can't touch a build already running.
    fn request_cancel(&mut self) {
        let Some(pipeline) = self.selected_pipeline.clone() else {
            self.status_line = "Open a pipeline first (enter) to cancel a running build".to_string();
            return;
        };
        let Some(inst) = self.history.first() else { return };
        let Some(stage) = inst.stages.iter().find(|s| s.is_active()) else {
            self.status_line = format!("Nothing currently running in {pipeline}");
            return;
        };
        let Some(stage_counter) = stage.counter.clone() else {
            self.error_line = Some("Stage counter unavailable, can't cancel".to_string());
            return;
        };
        let stage_name = stage.name.clone();
        let action =
            PendingAction::CancelStage { pipeline: pipeline.clone(), pipeline_counter: inst.counter, stage: stage_name.clone(), stage_counter };
        let message = format!("Cancel the running '{stage_name}' stage of '{pipeline}'?");
        self.modal = Some(Modal::Confirm { action, message });
    }

    fn toggle_favorite(&mut self) {
        let Some(name) = self.current_row_pipeline_name() else { return };
        if self.favorites.remove(&name) {
            self.status_line = format!("Removed {name} from favorites");
        } else {
            self.favorites.insert(name.clone());
            self.status_line = format!("Added {name} to favorites");
        }
        save_favorites(self.favorites.clone());
        self.rebuild_rows();
    }

    /// 'y': copy the most useful identifier for the current selection - the
    /// run's full commit SHA in history/details, the pipeline or group name
    /// in the tree.
    fn copy_selected(&mut self) {
        let (what, text) = match self.focus {
            Focus::History | Focus::Detail => {
                let Some(inst) = self.history.get(self.history_selected) else { return };
                match inst.git_modification().and_then(|m| m.revision.clone()) {
                    Some(sha) => ("commit sha", sha),
                    None => ("run label", inst.label.clone()),
                }
            }
            Focus::Groups => match self.rows.get(self.selected) {
                Some(Row::Pipeline { group_idx, pipeline_idx }) => {
                    let Some(name) = self.groups.get(*group_idx).and_then(|g| g.pipelines.get(*pipeline_idx)) else {
                        return;
                    };
                    ("pipeline name", name.clone())
                }
                Some(Row::FavoritePipeline(name)) => ("pipeline name", name.clone()),
                Some(Row::Group { idx }) => {
                    let Some(g) = self.groups.get(*idx) else { return };
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
                "https://github.com/{}/{}/compare/{}...{}",
                git_ref.owner, git_ref.repo, git_ref.deployed_sha, latest
            ),
            _ => format!("https://github.com/{}/{}/commit/{}", git_ref.owner, git_ref.repo, git_ref.deployed_sha),
        };
        match open_url(&url) {
            Ok(()) => self.status_line = format!("Opened {url}"),
            Err(e) => self.error_line = Some(format!("Couldn't open browser: {e}")),
        }
    }

    pub fn current_row_pipeline_name(&self) -> Option<String> {
        match self.focus {
            Focus::Groups => match self.rows.get(self.selected)? {
                Row::Pipeline { group_idx, pipeline_idx } => {
                    self.groups.get(*group_idx)?.pipelines.get(*pipeline_idx).cloned()
                }
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
                Some(Row::Pipeline { group_idx, pipeline_idx }) => {
                    if let Some(name) = self.groups.get(group_idx).and_then(|g| g.pipelines.get(pipeline_idx)).cloned()
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
                if let Some(DetailRow::Job(si, ji)) = self.detail_rows.get(self.detail_selected).copied() {
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
            self.github_ref = cached.first().and_then(|i| i.git_ref());
            self.history = cached;
            self.history_loading = false;
            self.status_line = format!("{name} (cached, refreshing...)");
            if let Some(git_ref) = self.github_ref.clone() {
                self.github_state = GithubState::Checking;
                self.spawn_check_github(name.clone(), &git_ref);
            } else {
                self.github_state = GithubState::Idle;
            }
        } else {
            self.history.clear();
            self.github_ref = None;
            self.github_state = GithubState::Idle;
            self.status_line = format!("Loading history for {name}...");
        }
        self.rebuild_detail_rows();
        self.detail_selected = 0;
        self.spawn_load_history(name);
    }

    fn open_console_log_for(&mut self, stage_idx: usize, job_idx: usize) {
        let Some(pipeline) = self.selected_pipeline.clone() else { return };
        let Some(inst) = self.history.get(self.history_selected) else { return };
        let Some(stage) = inst.stages.get(stage_idx) else { return };
        let Some(job) = stage.jobs.get(job_idx) else { return };
        let Some(stage_counter) = stage.counter.clone() else {
            self.error_line = Some("Stage counter unavailable, can't fetch console log".to_string());
            return;
        };
        let job_ref =
            JobRef { pipeline, pipeline_counter: inst.counter, stage: stage.name.clone(), stage_counter, job: job.name.clone() };
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
            title: format!("{}/{} - {}", job_ref.stage, job_ref.stage_counter, job_ref.job),
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
            artifacts: None,
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
                    if let Some(pos) = self.rows.iter().position(|r| matches!(r, Row::Group{idx} if *idx == group_idx)) {
                        self.selected = pos;
                    }
                }
            }
            Some(Row::FavoritePipeline(_)) => {
                self.favorites_expanded = false;
                self.rebuild_rows();
                if let Some(pos) = self.rows.iter().position(|r| matches!(r, Row::FavoritesHeader)) {
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
            Row::Pipeline { group_idx, pipeline_idx } => {
                Some(SelectionKey::Pipeline(self.groups.get(*group_idx)?.pipelines.get(*pipeline_idx)?.clone()))
            }
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
            (Row::Pipeline { group_idx, pipeline_idx }, SelectionKey::Pipeline(k)) => self
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
            let expanded = if filter.is_empty() { self.expanded.contains(&group.name) } else { true };
            if expanded {
                for pi in matching {
                    self.rows.push(Row::Pipeline { group_idx: gi, pipeline_idx: pi });
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
fn console_scroll_up(state: &mut ConsoleLogState, view_height: u16, lines: usize) {
    let content_len = match state.tab {
        JobTab::Materials => state.materials.len(),
        _ => state.lines.len(),
    };
    let max = content_len.saturating_sub(view_height as usize);
    state.scroll = state.scroll.min(max).saturating_sub(lines);
    state.following = false;
}

/// fzf-style subsequence match: every query char (spaces ignored) must appear
/// in order, case-insensitive. Returns the matched char positions for highlighting.
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<Vec<usize>> {
    let mut hay = haystack.chars().enumerate();
    needle
        .chars()
        .filter(|c| *c != ' ')
        .map(|nc| hay.by_ref().find(|(_, hc)| hc.eq_ignore_ascii_case(&nc)).map(|(i, _)| i))
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
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TBL[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TBL[n as usize & 63] as char } else { '=' });
    }
    out
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(not(target_os = "macos"))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ())
}

fn auth_hint(e: &str) -> &'static str {
    if e.contains("401") { "  (press A to reconnect)" } else { "" }
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
        assert_eq!(fuzzy_match("deploy-to-production", "dtp"), Some(vec![0, 7, 10]));
        assert_eq!(fuzzy_match("Deploy-Prod", "dep prod"), Some(vec![0, 1, 2, 7, 8, 9, 10]));
        assert_eq!(fuzzy_match("alpha", "px"), None);
        assert_eq!(fuzzy_match("abc", ""), Some(vec![]));
        // Chars must appear in order, not just anywhere.
        assert_eq!(fuzzy_match("ba", "ab"), None);
    }
}
