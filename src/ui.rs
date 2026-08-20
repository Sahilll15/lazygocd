use crate::app::{
    App, ConsoleLogState, DetailRow, Focus, GithubState, Modal, ReauthForm, ReauthMode, ReauthStep, Row as TreeRow,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row as TableRow, Table, Wrap,
};

/// A small, cohesive palette instead of scattered color literals - one place
/// to see (and change) what each color means, borrowing the semantic-color
/// convention common to lazygit/k9s/gh-dash: accent for focus/interactive,
/// success/warning/danger for state, muted for secondary text.
mod theme {
    use ratatui::style::Color;

    pub const ACCENT: Color = Color::Cyan;
    pub const SUCCESS: Color = Color::Green;
    pub const WARNING: Color = Color::Yellow;
    pub const DANGER: Color = Color::Red;
    pub const INFO: Color = Color::Magenta;
    pub const MUTED: Color = Color::DarkGray;
    /// Gold, distinct from WARNING yellow so a starred pipeline reads as "pinned",
    /// not "something's wrong with it".
    pub const FAVORITE: Color = Color::Rgb(255, 200, 40);
    /// Selected-row background, tinted toward ACCENT so the highlight reads
    /// as "the same cyan, just filled in" rather than an unrelated color.
    pub const SELECTED_BG: Color = Color::Rgb(20, 48, 58);
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)])
            .split(area);

    draw_header(f, app, chunks[0]);
    draw_statusbar(f, app, chunks[1]);
    draw_body(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);

    if app.filter_active {
        draw_filter_box(f, app, chunks[2]);
    }

    if let Some(modal) = app.modal.clone() {
        match modal {
            Modal::Help => draw_help(f, area),
            Modal::Confirm { message, .. } => draw_confirm(f, area, &message),
            Modal::Reauth(form) => draw_reauth(f, area, &form),
            Modal::GithubConnect { input } => draw_github_connect(f, area, &input),
            Modal::ConsoleLog(state) => app.console_view_height = draw_console_log(f, area, &state, app.tick),
        }
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let dot_color = if app.error_line.is_some() {
        theme::DANGER
    } else if app.loading_groups {
        theme::WARNING
    } else {
        theme::SUCCESS
    };

    let mut spans = vec![
        Span::styled(" lazygocd ", Style::default().fg(Color::Black).bg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("\u{25cf}", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(&app.server_url, Style::default().fg(theme::MUTED)),
    ];
    if let Some(err) = &app.error_line {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("! {err}"), Style::default().fg(theme::DANGER)));
    } else if !app.status_line.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(&app.status_line, Style::default().fg(theme::SUCCESS)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// k9s-style at-a-glance counts: only surfaces non-nominal states (paused/
/// building/failed) so a healthy fleet doesn't clutter the bar with zeros.
fn draw_statusbar(f: &mut Frame, app: &App, area: Rect) {
    if app.groups.is_empty() {
        return;
    }

    let mut paused = 0u32;
    let mut building = 0u32;
    let mut failed = 0u32;
    for p in app.pipeline_info.values() {
        if p.pause_info.paused {
            paused += 1;
        }
        match p.latest_status() {
            "Building" => building += 1,
            "Failed" => failed += 1,
            _ => {}
        }
    }

    let mut spans = vec![
        Span::styled(app.groups.len().to_string(), Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(" groups", Style::default().fg(theme::MUTED)),
        Span::raw("   "),
        Span::styled(
            app.pipeline_info.len().to_string(),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" pipelines", Style::default().fg(theme::MUTED)),
    ];
    if paused > 0 {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(format!("\u{23f8} {paused}"), Style::default().fg(theme::WARNING)));
        spans.push(Span::styled(" paused", Style::default().fg(theme::MUTED)));
    }
    if building > 0 {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(format!("\u{25d0} {building}"), Style::default().fg(theme::WARNING)));
        spans.push(Span::styled(" building", Style::default().fg(theme::MUTED)));
    }
    if failed > 0 {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            format!("\u{25cf} {failed}"),
            Style::default().fg(theme::DANGER).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" failed", Style::default().fg(theme::MUTED)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn footer_hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (key, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled((*key).to_string(), Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {desc}"), Style::default().fg(theme::MUTED)));
    }
    Line::from(spans)
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    const GROUPS_HINTS: &[(&str, &str)] = &[
        ("j/k", "move"),
        ("g/G", "top/bottom"),
        ("enter", "open"),
        ("h", "collapse"),
        ("t", "trigger"),
        ("p", "pause"),
        ("f", "favorite"),
        ("/", "filter"),
        ("r", "refresh"),
        ("A", "gocd"),
        ("@", "github"),
        ("?", "help"),
        ("q", "quit"),
    ];
    const HISTORY_HINTS: &[(&str, &str)] = &[
        ("j/k", "move"),
        ("g/G", "top/bottom"),
        ("tab", "detail"),
        ("esc", "back"),
        ("t", "trigger"),
        ("p", "pause"),
        ("f", "favorite"),
        ("X", "cancel"),
        ("r", "refresh"),
        ("A", "gocd"),
        ("@", "github"),
        ("?", "help"),
        ("q", "quit"),
    ];
    const DETAIL_HINTS: &[(&str, &str)] = &[
        ("j/k", "move"),
        ("g/G", "top/bottom"),
        ("enter", "console log"),
        ("tab", "groups"),
        ("esc", "back"),
        ("t", "trigger"),
        ("p", "pause"),
        ("f", "favorite"),
        ("X", "cancel"),
        ("r", "refresh"),
        ("?", "help"),
        ("q", "quit"),
    ];
    let hints = match app.focus {
        Focus::Groups => GROUPS_HINTS,
        Focus::History => HISTORY_HINTS,
        Focus::Detail => DETAIL_HINTS,
    };
    f.render_widget(Paragraph::new(footer_hints(hints)), area);
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).split(area);
    let right = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(cols[1]);
    // Remembered for mouse hit-testing (click-to-focus, click-to-select).
    app.tree_area = cols[0];
    app.history_area = right[0];
    app.detail_area = right[1];

    draw_tree(f, app, cols[0]);
    draw_history(f, app, right[0]);
    draw_detail(f, app, right[1]);
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Groups;
    let block = panel_block("Pipeline Groups", focused);

    if app.loading_groups && app.groups.is_empty() {
        f.render_widget(Paragraph::new(format!("{} Loading dashboard...", spinner(app.tick))).block(block), area);
        return;
    }
    if !app.loading_groups && app.groups.is_empty() && app.error_line.is_none() && app.modal.is_none() {
        f.render_widget(Paragraph::new("Press 'A' to connect to a GoCD server").block(block), area);
        return;
    }

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| match row {
            TreeRow::FavoritesHeader => {
                let arrow = if app.favorites_expanded { "\u{25be}" } else { "\u{25b8}" };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{arrow} \u{2605} Favorites"),
                        Style::default().fg(theme::FAVORITE).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" ({})", app.favorites.len()), Style::default().fg(theme::MUTED)),
                ]))
            }
            TreeRow::FavoritePipeline(name) => pipeline_list_item(app, name, false),
            TreeRow::Group { idx } => {
                let g = &app.groups[*idx];
                let arrow = if app.expanded.contains(&g.name) || !app.filter.is_empty() { "\u{25be}" } else { "\u{25b8}" };
                let mut spans = vec![
                    Span::styled(format!("{arrow} {}", g.name), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" ({})", g.pipelines.len()), Style::default().fg(theme::MUTED)),
                ];
                let (mut failed, mut building) = (0u32, 0u32);
                for name in &g.pipelines {
                    match app.pipeline_info.get(name).map(|p| p.latest_status()) {
                        Some("Failed") => failed += 1,
                        Some("Building") => building += 1,
                        _ => {}
                    }
                }
                if failed > 0 {
                    spans.push(Span::styled(format!(" \u{25cf}{failed}"), Style::default().fg(theme::DANGER)));
                }
                if building > 0 {
                    spans.push(Span::styled(format!(" \u{25d0}{building}"), Style::default().fg(theme::WARNING)));
                }
                ListItem::new(Line::from(spans))
            }
            TreeRow::Pipeline { group_idx, pipeline_idx } => {
                let name = &app.groups[*group_idx].pipelines[*pipeline_idx];
                pipeline_list_item(app, name, true)
            }
        })
        .collect();

    app.tree_state.select((!app.rows.is_empty()).then_some(app.selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(theme::SELECTED_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(list, area, &mut app.tree_state);
}

/// `show_star`: false when rendering inside the Favorites section itself, where
/// a star would be redundant - true for a pipeline's normal listing under its
/// real group, where the star is the only hint it's also favorited.
fn pipeline_list_item(app: &App, name: &str, show_star: bool) -> ListItem<'static> {
    let info = app.pipeline_info.get(name);
    let paused = info.map(|p| p.pause_info.paused).unwrap_or(false);
    let locked = info.map(|p| p.locked).unwrap_or(false);

    let (icon, icon_style) = if paused {
        ("\u{23f8}", Style::default().fg(theme::WARNING))
    } else if let Some(info) = info {
        let s = info.latest_status();
        (dot_for(s), Style::default().fg(status_color(s)))
    } else {
        ("\u{2022}", Style::default().fg(theme::MUTED))
    };

    let mut spans = vec![Span::raw("   "), Span::styled(icon, icon_style), Span::raw(" ")];
    if show_star && app.favorites.contains(name) {
        spans.push(Span::styled("\u{2605} ", Style::default().fg(theme::FAVORITE)));
    }
    spans.extend(name_spans(name, &app.filter));
    if locked {
        spans.push(Span::styled(" \u{1f512}", Style::default().fg(theme::MUTED)));
    }
    ListItem::new(Line::from(spans))
}

/// Pipeline name with fuzzy-matched chars highlighted while a filter is set.
fn name_spans(name: &str, filter: &str) -> Vec<Span<'static>> {
    let matched = (!filter.is_empty()).then(|| crate::app::fuzzy_match(name, filter)).flatten();
    let Some(matched) = matched else { return vec![Span::raw(name.to_string())] };

    let hit_style = Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_is_hit = false;
    let mut next_hit = matched.iter().peekable();
    for (i, c) in name.chars().enumerate() {
        let is_hit = next_hit.peek() == Some(&&i);
        if is_hit {
            next_hit.next();
        }
        if is_hit != run_is_hit && !run.is_empty() {
            spans.push(styled_run(std::mem::take(&mut run), run_is_hit, hit_style));
        }
        run_is_hit = is_hit;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(styled_run(run, run_is_hit, hit_style));
    }
    spans
}

fn styled_run(text: String, is_hit: bool, hit_style: Style) -> Span<'static> {
    if is_hit { Span::styled(text, hit_style) } else { Span::raw(text) }
}

fn dot_for(status: &str) -> &'static str {
    match status {
        "Passed" => "\u{25cf}",
        "Failed" => "\u{25cf}",
        "Building" => "\u{25d0}",
        "Cancelled" => "\u{25d1}",
        _ => "\u{25cb}",
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "Passed" => theme::SUCCESS,
        "Failed" => theme::DANGER,
        "Building" => theme::WARNING,
        "Cancelled" => theme::INFO,
        _ => theme::MUTED,
    }
}

fn draw_history(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::History;
    let title = match &app.selected_pipeline {
        Some(name) => format!("History: {name}"),
        None => "History".to_string(),
    };
    let block = panel_block(title, focused);

    if app.selected_pipeline.is_none() {
        f.render_widget(Paragraph::new("Select a pipeline (enter) to view its history").block(block), area);
        return;
    }
    if app.history_loading && app.history.is_empty() {
        f.render_widget(Paragraph::new(format!("{} Loading history...", spinner(app.tick))).block(block), area);
        return;
    }
    if app.history.is_empty() {
        f.render_widget(Paragraph::new("No runs yet").block(block), area);
        return;
    }

    let rows: Vec<TableRow> = app
        .history
        .iter()
        .map(|inst| {
            let status = inst.overall_status();
            let cause = inst
                .build_cause
                .as_ref()
                .and_then(|b| b.trigger_message.clone())
                .unwrap_or_default();
            let when = inst.scheduled_date.map(format_ts).unwrap_or_default();
            TableRow::new(vec![
                Cell::from(format!("#{}", inst.label)),
                Cell::from(when),
                Cell::from(Span::styled(status, Style::default().fg(status_color(status)))),
                Cell::from(cause),
            ])
        })
        .collect();

    let widths = [Constraint::Length(10), Constraint::Length(20), Constraint::Length(12), Constraint::Min(10)];
    let table = Table::new(rows, widths)
        .header(
            TableRow::new(vec!["Run", "When", "Status", "Trigger"])
                .style(Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD)),
        )
        .block(block)
        .row_highlight_style(Style::default().bg(theme::SELECTED_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("\u{25b6} ");

    app.history_state.select(Some(app.history_selected));
    f.render_stateful_widget(table, area, &mut app.history_state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let block = panel_block("Details", focused);

    let Some(inst) = app.history.get(app.history_selected) else {
        f.render_widget(Paragraph::new("No run selected").block(block), area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![label("Run "), Span::raw(format!("#{}", inst.label))]));
    if let Some(ts) = inst.scheduled_date {
        lines.push(Line::from(vec![label("Started: "), Span::raw(format_ts(ts))]));
    }
    if let Some(comment) = &inst.comment {
        lines.push(Line::from(vec![label("Comment: "), Span::raw(comment.clone())]));
    }
    if let Some(cause) = &inst.build_cause {
        if let Some(msg) = &cause.trigger_message {
            lines.push(Line::from(vec![label("Cause: "), Span::raw(msg.clone())]));
        }
        if let Some(approver) = &cause.approver {
            lines.push(Line::from(vec![label("By: "), Span::raw(approver.clone())]));
        }
    }
    if let Some(m) = inst.git_modification() {
        if let Some(sha) = &m.revision {
            lines.push(Line::from(vec![
                label("Commit: "),
                Span::styled(short_sha(sha), Style::default().fg(theme::ACCENT)),
                Span::raw("  "),
                Span::raw(first_line(m.comment.as_deref().unwrap_or(""))),
            ]));
        }
        if let Some(author) = &m.user_name {
            lines.push(Line::from(vec![label("Author: "), Span::raw(author.clone())]));
        }
        if let Some(ts) = m.modified_time {
            lines.push(Line::from(vec![label("Committed: "), Span::raw(format_ts(ts))]));
        }
    }
    lines.push(Line::from(""));

    let selected_row = app.detail_rows.get(app.detail_selected).copied();
    for (si, stage) in inst.stages.iter().enumerate() {
        let status = stage.status.as_deref().unwrap_or("Unknown");
        let stage_selected = focused && matches!(selected_row, Some(DetailRow::Stage(s)) if s == si);
        let (cursor, cursor_style) = if stage_selected {
            ("\u{25b6} ", Style::default().fg(theme::ACCENT))
        } else {
            ("\u{25b8} ", Style::default().fg(theme::MUTED))
        };
        let mut stage_line = vec![
            Span::styled(cursor, cursor_style),
            Span::styled(stage.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(status, Style::default().fg(status_color(status))),
        ];
        if let Some(approval) = &stage.approval_type {
            stage_line.push(Span::styled(format!("  ({approval})"), Style::default().fg(theme::MUTED)));
        }
        if let Some(ts) = stage.scheduled_date {
            stage_line.push(Span::styled(format!("  {}", format_ts(ts)), Style::default().fg(theme::MUTED)));
        }
        lines.push(Line::from(stage_line));
        for (ji, job) in stage.jobs.iter().enumerate() {
            let job_selected = focused && matches!(selected_row, Some(DetailRow::Job(s, j)) if s == si && j == ji);
            let jresult = job.result.as_deref().or(job.state.as_deref()).unwrap_or("-");
            let (prefix, name_style) = if job_selected {
                ("\u{25b6} ", Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ("    ", Style::default())
            };
            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(job.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(jresult, Style::default().fg(status_color(jresult))),
            ]));
        }
    }

    // The GitHub comparison only applies to the latest run's material, not whichever
    // older row happens to be selected in the history table above.
    if app.history_selected == 0
        && let Some(git_ref) = &app.github_ref
    {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            label("GitHub: "),
            Span::raw(format!("{}/{}@{}", git_ref.owner, git_ref.repo, git_ref.branch)),
        ]));
        let deployed_short = short_sha(&git_ref.deployed_sha);
        let status_span = match &app.github_state {
            GithubState::Idle => Span::styled("(not checked)", Style::default().fg(theme::MUTED)),
            GithubState::Checking => {
                Span::styled(format!("{} checking...", spinner(app.tick)), Style::default().fg(theme::MUTED))
            }
            GithubState::Found(latest) if latest == &git_ref.deployed_sha => {
                Span::styled("\u{2713} up to date", Style::default().fg(theme::SUCCESS))
            }
            GithubState::Found(latest) => Span::styled(
                format!("\u{26a0} not latest (latest {})", short_sha(latest)),
                Style::default().fg(theme::WARNING),
            ),
            GithubState::Failed(_) => {
                Span::styled("can't check (connect GitHub with '@')", Style::default().fg(theme::MUTED))
            }
        };
        lines.push(Line::from(vec![label("Deployed: "), Span::raw(deployed_short), Span::raw("  "), status_span]));
    }

    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn label(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(theme::MUTED))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

const SPINNER_FRAMES: [char; 8] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}'];

/// tick increments every ~40ms; divide down so frames change roughly every 160ms.
fn spinner(tick: u64) -> char {
    SPINNER_FRAMES[(tick / 4) as usize % SPINNER_FRAMES.len()]
}

/// Epoch millis (as GoCD sends them) to a local-time "YYYY-MM-DD HH:MM:SS" string.
fn format_ts(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string(),
        None => "-".to_string(),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

fn draw_filter_box(f: &mut Frame, app: &App, area: Rect) {
    let rect = Rect { x: area.x + 2, y: area.y, width: (area.width.saturating_sub(4)).min(50), height: 3 };
    f.render_widget(Clear, rect);
    let block = styled_block("Filter", theme::ACCENT);
    f.render_widget(Paragraph::new(format!("/{}", app.filter)).block(block), rect);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rect = centered_rect(70, 85, area);
    f.render_widget(Clear, rect);
    let text = vec![
        Line::from(Span::styled("lazygocd keybindings", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from("j/k, \u{2193}/\u{2191}     move selection"),
        Line::from("g/G          jump to top / bottom of the focused list"),
        Line::from("ctrl-d/u     half-page down / up (also pgdn/pgup)"),
        Line::from("l/enter/\u{2192}    expand group / open pipeline history / open console log"),
        Line::from("h/\u{2190}          collapse group"),
        Line::from("tab          cycle focus: groups -> history -> details -> groups"),
        Line::from("esc          back: details -> history -> groups; in groups, clear filter"),
        Line::from("t            trigger selected pipeline"),
        Line::from("p            pause/unpause selected pipeline"),
        Line::from("f            star/unstar selected pipeline as a favorite"),
        Line::from("X            cancel the currently running stage"),
        Line::from("/            fuzzy-filter pipelines by name"),
        Line::from("r            refresh"),
        Line::from("A            connect / reconnect GoCD"),
        Line::from("@            connect GitHub (optional, for private-repo checks)"),
        Line::from("?            toggle this help"),
        Line::from("q / ctrl-c   quit"),
        Line::from(""),
        Line::from(Span::styled("mouse:", Style::default().fg(theme::MUTED))),
        Line::from("click        focus pane / select row; click a selected row to open it"),
        Line::from("wheel        scroll the pane under the cursor (or the console log)"),
        Line::from(""),
        Line::from(Span::styled("in the details pane:", Style::default().fg(theme::MUTED))),
        Line::from("enter        open the selected job's console log"),
        Line::from(Span::styled("in the console log view:", Style::default().fg(theme::MUTED))),
        Line::from("j/k          scroll   g/G top/bottom   r refresh   q/esc close"),
        Line::from(""),
        Line::from(Span::styled("press any key to close", Style::default().fg(theme::MUTED))),
    ];
    let block = styled_block("Help", theme::ACCENT);
    f.render_widget(Paragraph::new(text).block(block), rect);
}

fn draw_confirm(f: &mut Frame, area: Rect, message: &str) {
    let rect = centered_rect(50, 20, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Confirm", theme::WARNING);
    let text = vec![
        Line::from(message.to_string()),
        Line::from(""),
        Line::from(Span::styled("y/enter confirm   n/esc cancel", Style::default().fg(theme::MUTED))),
    ];
    f.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), rect);
}

/// Returns the content viewport height so scroll-up clamping can use it.
fn draw_console_log(f: &mut Frame, area: Rect, state: &ConsoleLogState, tick: u64) -> u16 {
    let rect = centered_rect(90, 88, area);
    f.render_widget(Clear, rect);
    let block = styled_block(&state.title, theme::ACCENT);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::vertical([Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)]).split(inner);
    let (content_area, status_area, hint_area) = (layout[0], layout[1], layout[2]);

    if state.lines.is_empty() && state.loading {
        f.render_widget(Paragraph::new(format!("{} Loading console log...", spinner(tick))), content_area);
    } else if state.lines.is_empty() {
        let msg = state.error.as_deref().unwrap_or("No output yet");
        f.render_widget(Paragraph::new(Span::styled(msg.to_string(), Style::default().fg(theme::MUTED))), content_area);
    } else {
        let visible = content_area.height as usize;
        let max_scroll = state.lines.len().saturating_sub(visible);
        let scroll = state.scroll.min(max_scroll);
        let text: Vec<Line> = state.lines.iter().map(|l| Line::from(strip_console_markers(l))).collect();
        f.render_widget(Paragraph::new(text).scroll((scroll as u16, 0)), content_area);
    }

    let mut status_spans = vec![Span::styled(format!("{} lines", state.lines.len()), Style::default().fg(theme::MUTED))];
    if state.loading {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(format!("{} refreshing", spinner(tick)), Style::default().fg(theme::MUTED)));
    }
    if state.following {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled("following", Style::default().fg(theme::SUCCESS)));
    }
    if let Some(err) = &state.error {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(format!("! {err}"), Style::default().fg(theme::DANGER)));
    }
    f.render_widget(Paragraph::new(Line::from(status_spans)), status_area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k scroll   g/G top/bottom   r refresh   q/esc close",
            Style::default().fg(theme::MUTED),
        ))),
        hint_area,
    );
    content_area.height
}

/// GoCD prefixes each console line with a 2-char marker + '|' (e.g. "##|", "&2|")
/// indicating stdout/stderr/task-boundary framing - noise for a plain log view.
fn strip_console_markers(line: &str) -> &str {
    if line.len() > 3 && line.as_bytes()[2] == b'|' { &line[3..] } else { line }
}

fn draw_github_connect(f: &mut Frame, area: Rect, input: &str) {
    let rect = centered_rect(60, 45, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Connect GitHub", theme::ACCENT);

    let lines = vec![
        field_label("GitHub personal access token"),
        Line::from(Span::styled(
            "optional - only needed to check private repos; leave blank to disconnect",
            Style::default().fg(theme::MUTED),
        )),
        Line::from(""),
        input_line(input, true),
        Line::from(""),
        Line::from(Span::styled("enter save   esc cancel", Style::default().fg(theme::MUTED))),
    ];
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), rect);
}

fn draw_reauth(f: &mut Frame, area: Rect, form: &ReauthForm) {
    let rect = centered_rect(64, 60, area);
    f.render_widget(Clear, rect);

    let title = match form.mode {
        ReauthMode::Connect => "Connect to GoCD",
        ReauthMode::Reconnect => "Reconnect to GoCD",
    };
    let block = styled_block(title, theme::ACCENT);

    let mut lines: Vec<Line> = Vec::new();

    let mut wrote_summary = false;
    if form.step != ReauthStep::ServerUrl {
        lines.push(summary_line("Server", &form.server_url));
        wrote_summary = true;
    }
    if matches!(form.step, ReauthStep::Username | ReauthStep::Secret | ReauthStep::Insecure) {
        let method = if form.use_token { "Access token" } else { "Username & password" };
        lines.push(summary_line("Method", method));
        wrote_summary = true;
    }
    if matches!(form.step, ReauthStep::Secret | ReauthStep::Insecure) && !form.use_token && !form.username.is_empty() {
        lines.push(summary_line("User", &form.username));
        wrote_summary = true;
    }
    if wrote_summary {
        lines.push(Line::from(""));
    }

    match form.step {
        ReauthStep::ServerUrl => {
            lines.push(field_label("GoCD server URL"));
            lines.push(Line::from(Span::styled("e.g. https://gocd.example.com/go", Style::default().fg(theme::MUTED))));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, false));
        }
        ReauthStep::ChooseAuthMethod => {
            lines.push(field_label("Authenticate with"));
            lines.push(Line::from(""));
            lines.push(choice_line("Username & password", form.choice_index == 0));
            lines.push(choice_line("Access token (recommended)", form.choice_index == 1));
        }
        ReauthStep::Username => {
            lines.push(field_label("Username"));
            lines.push(Line::from(Span::styled("leave blank if the server has no auth", Style::default().fg(theme::MUTED))));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, false));
        }
        ReauthStep::Secret => {
            lines.push(field_label(if form.use_token { "Access token" } else { "Password" }));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, true));
        }
        ReauthStep::Insecure => {
            lines.push(field_label("Skip TLS certificate verification?"));
            lines.push(Line::from(Span::styled("only for self-signed certs", Style::default().fg(theme::MUTED))));
            lines.push(Line::from(""));
            lines.push(choice_line("No (recommended)", form.choice_index == 0));
            lines.push(choice_line("Yes", form.choice_index == 1));
        }
    }

    lines.push(Line::from(""));
    let hint = if form.step.is_choice() {
        "\u{2191}/\u{2193} or j/k select   enter confirm   esc cancel"
    } else {
        "enter confirm   esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, Style::default().fg(theme::MUTED))));

    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), rect);
}

fn summary_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("\u{2713} ", Style::default().fg(theme::SUCCESS)),
        Span::styled(format!("{label}: "), Style::default().fg(theme::MUTED)),
        Span::raw(value.to_string()),
    ])
}

fn field_label(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::default().add_modifier(Modifier::BOLD)))
}

fn input_line(input: &str, masked: bool) -> Line<'static> {
    let shown = if masked { "\u{2022}".repeat(input.chars().count()) } else { input.to_string() };
    Line::from(Span::styled(format!("> {shown}"), Style::default().fg(theme::ACCENT)))
}

fn choice_line(label: &str, selected: bool) -> Line<'static> {
    if selected {
        Line::from(Span::styled(
            format!("\u{25b6} {label}"),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(format!("  {label}"), Style::default().fg(theme::MUTED)))
    }
}

fn styled_block(title: impl std::fmt::Display, color: Color) -> Block<'static> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
}

fn panel_block(title: impl std::fmt::Display, focused: bool) -> Block<'static> {
    styled_block(title, if focused { theme::ACCENT } else { theme::MUTED })
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
