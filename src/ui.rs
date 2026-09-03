use crate::app::{
    App, ConsoleLogState, DetailRow, Focus, GithubState, JobTab, Modal, ReauthForm, ReauthMode,
    ReauthStep, Row as TreeRow, TriggerVarsForm,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row as TableRow, Table,
    Wrap,
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
    // A rejected certificate names both fixes and runs long. Without a second row
    // on a narrow terminal, the half naming the setting is the half that is cut.
    let message_rows = match &app.error_line {
        Some(e) if e.chars().count() as u16 > area.width.saturating_sub(2) => 2,
        _ => 1,
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(message_rows),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
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
            Modal::TriggerVars(form) => draw_trigger_vars(f, area, &form),
            Modal::ConsoleLog(state) => {
                app.console_view_height = draw_console_log(f, area, &state, app.tick)
            }
            Modal::ViewPicker { selected } => draw_view_picker(f, area, app, selected),
            Modal::SaveView { input, pipelines } => {
                draw_save_view(f, area, &input, pipelines.len())
            }
        }
    }
}

/// Host only: the full URL never changes mid-session and ate 45 columns.
fn short_host(url: &str) -> String {
    let s = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    s.split('/').next().unwrap_or(s).to_string()
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
        Span::styled(
            " lazygocd ",
            Style::default()
                .fg(Color::Black)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("\u{25cf}", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(short_host(&app.server_url), Style::default().fg(theme::MUTED)),
    ];

    if !app.groups.is_empty() {
        let (paused, building, failed) = app.fleet_counts();
        spans.push(Span::raw("   "));
        spans.push(count_span(app.groups.len(), "groups", theme::ACCENT));
        spans.push(Span::styled(" \u{00b7} ", Style::default().fg(theme::MUTED)));
        spans.push(count_span(app.pipeline_info.len(), "pipelines", theme::ACCENT));
        if paused > 0 {
            spans.push(Span::styled(" \u{00b7} ", Style::default().fg(theme::MUTED)));
            spans.push(Span::styled(
                format!("\u{23f8}{paused}"),
                Style::default().fg(theme::WARNING),
            ));
        }
        if building > 0 {
            spans.push(Span::styled(" \u{00b7} ", Style::default().fg(theme::MUTED)));
            spans.push(Span::styled(
                format!("\u{25d0}{building}"),
                Style::default().fg(theme::WARNING),
            ));
        }
        if failed > 0 {
            spans.push(Span::styled(" \u{00b7} ", Style::default().fg(theme::MUTED)));
            spans.push(Span::styled(
                format!("\u{25cf}{failed}"),
                Style::default()
                    .fg(theme::DANGER)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    if !app.filter.is_empty() {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            format!("/{}", app.filter),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            if app.search_groups { " +groups" } else { "" },
            Style::default().fg(theme::MUTED),
        ));
    }

    if let Some(view) = app.active_view.and_then(|i| app.views.get(i)) {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            format!("[{}]", view.name),
            Style::default()
                .fg(theme::FAVORITE)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn count_span(n: usize, label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!("{n} {label}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// The message row. Errors get the whole width here instead of being appended
/// to the header and sliced off mid-sentence.
fn draw_statusbar(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(err) = &app.error_line {
        Line::from(vec![
            Span::styled(
                "\u{25bc} ",
                Style::default().fg(theme::DANGER).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err.clone(), Style::default().fg(theme::DANGER)),
        ])
    } else if !app.status_line.is_empty() {
        Line::from(Span::styled(
            app.status_line.clone(),
            Style::default().fg(theme::SUCCESS),
        ))
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: true }), area);
}

/// key, label, and whether the key changes server state (rendered red so a
/// destructive action never hides among navigation keys).
type Hint = (&'static str, &'static str, bool);

fn footer_hints(pane: &str, pairs: &[Hint]) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!(" {pane} "),
            Style::default()
                .fg(Color::Black)
                .bg(theme::MUTED)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    for (i, (key, desc, mutates)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let key_color = if *mutates { theme::DANGER } else { theme::ACCENT };
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default().fg(key_color).add_modifier(Modifier::BOLD),
        ));
        let desc_color = if *mutates { theme::DANGER } else { theme::MUTED };
        spans.push(Span::styled(
            format!(" {desc}"),
            Style::default().fg(desc_color),
        ));
    }
    Line::from(spans)
}

/// Six hints max so the row survives an 80-column terminal; the full list
/// lives behind '?'.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    const GROUPS_HINTS: &[Hint] = &[
        ("j/k", "move", false),
        ("enter", "open", false),
        ("1/2/3", "pane", false),
        ("//^g", "filter", false),
        ("O", "gocd", false),
        ("t", "trigger", true),
        ("?", "keys", false),
    ];
    const HISTORY_HINTS: &[Hint] = &[
        ("j/k", "move", false),
        ("1/2/3", "pane", false),
        ("o", "commit", false),
        ("O", "gocd", false),
        ("R", "rerun", true),
        ("X", "cancel", true),
        ("?", "keys", false),
    ];
    const DETAIL_HINTS: &[Hint] = &[
        ("j/k", "move", false),
        ("enter", "console", false),
        ("1/2/3", "pane", false),
        ("O", "gocd", false),
        ("R", "rerun", true),
        ("X", "cancel", true),
        ("?", "keys", false),
    ];
    let (pane, hints) = match app.focus {
        Focus::Groups => ("groups", GROUPS_HINTS),
        Focus::History => ("history", HISTORY_HINTS),
        Focus::Detail => ("jobs", DETAIL_HINTS),
    };
    f.render_widget(Paragraph::new(footer_hints(pane, hints)), area);
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    let cols =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).split(area);
    let right =
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(cols[1]);
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
        f.render_widget(
            Paragraph::new(format!("{} Loading dashboard...", spinner(app.tick))).block(block),
            area,
        );
        return;
    }
    if !app.loading_groups
        && app.groups.is_empty()
        && app.error_line.is_none()
        && app.modal.is_none()
    {
        f.render_widget(
            Paragraph::new("Press 'A' to connect to a GoCD server").block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| match row {
            TreeRow::FavoritesHeader => {
                let arrow = if app.favorites_expanded {
                    "\u{25be}"
                } else {
                    "\u{25b8}"
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{arrow} \u{2605} Favorites"),
                        Style::default()
                            .fg(theme::FAVORITE)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" ({})", app.favorites.len()),
                        Style::default().fg(theme::MUTED),
                    ),
                ]))
            }
            TreeRow::FavoritePipeline(name) => pipeline_list_item(app, name, false),
            TreeRow::Group { idx } => {
                let g = &app.groups[*idx];
                let arrow = if app.expanded.contains(&g.name) || !app.filter.is_empty() {
                    "\u{25be}"
                } else {
                    "\u{25b8}"
                };
                let mut spans = vec![
                    Span::styled(
                        format!("{arrow} {}", g.name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" ({})", g.pipelines.len()),
                        Style::default().fg(theme::MUTED),
                    ),
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
                    spans.push(Span::styled(
                        format!(" \u{25cf}{failed}"),
                        Style::default().fg(theme::DANGER),
                    ));
                }
                if building > 0 {
                    spans.push(Span::styled(
                        format!(" \u{25d0}{building}"),
                        Style::default().fg(theme::WARNING),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
            TreeRow::Pipeline {
                group_idx,
                pipeline_idx,
            } => {
                let name = &app.groups[*group_idx].pipelines[*pipeline_idx];
                pipeline_list_item(app, name, true)
            }
        })
        .collect();

    app.tree_state
        .select((!app.rows.is_empty()).then_some(app.selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(theme::SELECTED_BG)
                .add_modifier(Modifier::BOLD),
        )
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

    let mut spans = vec![
        Span::raw("   "),
        Span::styled(icon, icon_style),
        Span::raw(" "),
    ];
    if show_star && app.favorites.contains(name) {
        spans.push(Span::styled(
            "\u{2605} ",
            Style::default().fg(theme::FAVORITE),
        ));
    }
    spans.extend(name_spans(name, &app.filter));
    if locked {
        spans.push(Span::styled(
            " \u{1f512}",
            Style::default().fg(theme::MUTED),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// Pipeline name with fuzzy-matched chars highlighted while a filter is set.
fn name_spans(name: &str, filter: &str) -> Vec<Span<'static>> {
    let matched = (!filter.is_empty())
        .then(|| crate::app::fuzzy_match(name, filter))
        .flatten();
    let Some(matched) = matched else {
        return vec![Span::raw(name.to_string())];
    };

    let hit_style = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);
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
    if is_hit {
        Span::styled(text, hit_style)
    } else {
        Span::raw(text)
    }
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
        f.render_widget(
            Paragraph::new("Select a pipeline (enter) to view its history").block(block),
            area,
        );
        return;
    }
    if app.history_loading && app.history.is_empty() {
        f.render_widget(
            Paragraph::new(format!("{} Loading history...", spinner(app.tick))).block(block),
            area,
        );
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
                Cell::from(format!("#{} {}", inst.counter, inst.label)),
                Cell::from(when),
                Cell::from(Span::styled(
                    status,
                    Style::default().fg(status_color(status)),
                )),
                Cell::from(cause),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(15),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .header(
            TableRow::new(vec!["Run", "When", "Status", "Trigger"]).style(
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(theme::SELECTED_BG)
                .add_modifier(Modifier::BOLD),
        )
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
    lines.push(Line::from(vec![
        label("Run "),
        Span::raw(format!("#{} {}", inst.counter, inst.label)),
    ]));
    if let Some(ts) = inst.scheduled_date {
        lines.push(Line::from(vec![
            label("Started: "),
            Span::raw(format_ts(ts)),
        ]));
    }
    if let Some(comment) = &inst.comment {
        lines.push(Line::from(vec![
            label("Comment: "),
            Span::raw(comment.clone()),
        ]));
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
            lines.push(Line::from(vec![
                label("Author: "),
                Span::raw(author.clone()),
            ]));
        }
        if let Some(ts) = m.modified_time {
            lines.push(Line::from(vec![
                label("Committed: "),
                Span::raw(format_ts(ts)),
            ]));
        }
    }
    lines.push(Line::from(""));

    let selected_row = app.detail_rows.get(app.detail_selected).copied();
    // Where the selected row lands in `lines`, so the pane can scroll to it.
    let mut selected_line: Option<usize> = None;
    for (si, stage) in inst.stages.iter().enumerate() {
        let status = stage.status.as_deref().unwrap_or("Unknown");
        let stage_selected =
            focused && matches!(selected_row, Some(DetailRow::Stage(s)) if s == si);
        let (cursor, cursor_style) = if stage_selected {
            ("\u{25b6} ", Style::default().fg(theme::ACCENT))
        } else {
            ("\u{25b8} ", Style::default().fg(theme::MUTED))
        };
        let mut stage_line = vec![
            Span::styled(cursor, cursor_style),
            Span::styled(
                stage.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(status, Style::default().fg(status_color(status))),
        ];
        if let Some(approval) = &stage.approval_type {
            stage_line.push(Span::styled(
                format!("  ({approval})"),
                Style::default().fg(theme::MUTED),
            ));
        }
        if let Some(ts) = stage.scheduled_date {
            stage_line.push(Span::styled(
                format!("  {}", format_ts(ts)),
                Style::default().fg(theme::MUTED),
            ));
        }
        if stage_selected {
            selected_line = Some(lines.len());
        }
        lines.push(Line::from(stage_line));
        for (ji, job) in stage.jobs.iter().enumerate() {
            let job_selected =
                focused && matches!(selected_row, Some(DetailRow::Job(s, j)) if s == si && j == ji);
            let jresult = job
                .result
                .as_deref()
                .or(job.state.as_deref())
                .unwrap_or("-");
            let (prefix, name_style) = if job_selected {
                (
                    "\u{25b6} ",
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("    ", Style::default())
            };
            if job_selected {
                selected_line = Some(lines.len());
            }
            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(job.name.clone(), name_style),
                Span::raw("  "),
                Span::styled(jresult, Style::default().fg(status_color(jresult))),
            ]));
        }
    }

    // The GitHub comparison only applies to the latest run's materials, not whichever
    // older row happens to be selected in the history table above.
    if app.history_selected == 0 && !app.github_checks.is_empty() {
        lines.push(Line::from(""));
        for (git_ref, state) in &app.github_checks {
            // Only surface non-github.com hosts; the default host is just noise.
            let host = if git_ref.host == "github.com" {
                String::new()
            } else {
                format!("{}/", git_ref.host)
            };
            let status_span = match state {
                GithubState::Idle => {
                    Span::styled("(not checked)", Style::default().fg(theme::MUTED))
                }
                GithubState::Checking => Span::styled(
                    format!("{} checking...", spinner(app.tick)),
                    Style::default().fg(theme::MUTED),
                ),
                GithubState::Found(latest) if latest == &git_ref.deployed_sha => {
                    Span::styled("\u{2713} up to date", Style::default().fg(theme::SUCCESS))
                }
                GithubState::Found(latest) => Span::styled(
                    format!("\u{26a0} not latest (latest {})", short_sha(latest)),
                    Style::default().fg(theme::WARNING),
                ),
                // Was a flat "connect GitHub with '@'", which is wrong advice when
                // the token is present and merely rejected. The full reason is in
                // the message row; this is the short label that fits here.
                GithubState::Failed(why) => Span::styled(
                    format!("\u{2717} {}", github_failure_label(why)),
                    Style::default().fg(theme::WARNING),
                ),
            };
            lines.push(Line::from(vec![
                label("GitHub: "),
                Span::raw(format!(
                    "{host}{}/{}@{}",
                    git_ref.owner, git_ref.repo, git_ref.branch
                )),
                Span::raw("  "),
                Span::styled(
                    short_sha(&git_ref.deployed_sha),
                    Style::default().fg(theme::ACCENT),
                ),
                Span::raw("  "),
                status_span,
            ]));
            // Say so when the commit was traced upstream, otherwise it reads as
            // if this deploy run had a Git material of its own.
            if let Some((up_name, up_counter)) = &git_ref.via {
                lines.push(Line::from(Span::styled(
                    format!("        via {up_name} #{up_counter}"),
                    Style::default().fg(theme::MUTED),
                )));
            }
        }
    }

    // A run with many stages and jobs overflows the pane. Scroll to keep the
    // selected row on screen: without this the cursor moved invisibly and
    // 'enter' opened a log you could not see you had selected.
    let inner_height = area.height.saturating_sub(2) as usize;
    let offset = match selected_line {
        Some(line) if inner_height > 0 && line >= inner_height => {
            (line - inner_height + 1).min(lines.len())
        }
        _ => 0,
    };

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((offset as u16, 0)),
        area,
    );
}

fn label(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(theme::MUTED))
}

fn github_failure_label(why: &str) -> &'static str {
    if why.contains("SSO") {
        "needs SSO authorization"
    } else if why.contains("401") {
        "token invalid"
    } else if why.contains("404") {
        "repo or branch not found"
    } else if why.contains("no token") || why.contains("rate limit") {
        "no token (press @ to connect)"
    } else {
        "check failed"
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

const SPINNER_FRAMES: [char; 8] = [
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
];

/// tick increments every ~40ms; divide down so frames change roughly every 160ms.
fn spinner(tick: u64) -> char {
    SPINNER_FRAMES[(tick / 4) as usize % SPINNER_FRAMES.len()]
}

/// Epoch millis (as GoCD sends them) to a local-time "YYYY-MM-DD HH:MM:SS" string.
fn format_ts(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => "-".to_string(),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

fn draw_filter_box(f: &mut Frame, app: &App, area: Rect) {
    let rect = Rect {
        x: area.x + 2,
        y: area.y,
        width: (area.width.saturating_sub(4)).min(50),
        height: 3,
    };
    f.render_widget(Clear, rect);
    let scope = if app.search_groups { "on" } else { "off" };
    let block = styled_block(format!("Filter  ^g groups:{scope}"), theme::ACCENT);
    f.render_widget(
        Paragraph::new(format!("/{}", app.filter)).block(block),
        rect,
    );
}

/// Grouped into four task blocks across two columns. The old single column ran
/// 35 lines and overflowed its own box.
fn draw_help(f: &mut Frame, area: Rect) {
    const MOVE: &[Hint] = &[
        ("j/k \u{2193}\u{2191}", "row", false),
        ("g/G", "top / bottom", false),
        ("ctrl-d/u", "half page", false),
        ("tab", "next pane", false),
        ("1 2 3", "tree/history/jobs", false),
        ("enter", "open", false),
        ("h/esc", "collapse / back", false),
        ("/", "filter pipelines", false),
        ("^g", "incl. group names", false),
    ];
    const JOB: &[Hint] = &[
        ("1 2 3", "console/art/mat", false),
        ("tab", "cycle tabs", false),
        ("/", "search log", false),
        ("n/N", "next / prev hit", false),
        ("g/G", "top / bottom", false),
        ("e", "open in $EDITOR", false),
        ("y", "copy artifact url", false),
        ("q/esc", "close", false),
    ];
    const MUTATE: &[Hint] = &[
        ("t", "trigger", true),
        ("T", "trigger + vars", true),
        ("R", "rerun failed", true),
        ("X", "cancel stage", true),
        ("p", "pause / unpause", true),
        ("V", "save view", true),
    ];
    const SERVER: &[Hint] = &[
        ("A", "reconnect GoCD", false),
        ("@", "GitHub token", false),
        ("v", "switch view", false),
        ("r", "refresh", false),
        ("f", "favorite", false),
        ("y", "copy sha / name", false),
        ("o", "open commit", false),
        ("O", "open in GoCD", false),
    ];

    let rect = centered_rect(72, 62, area);
    f.render_widget(Clear, rect);

    let mut text = vec![
        help_headings("MOVE", theme::ACCENT, "JOB & LOGS", theme::ACCENT),
    ];
    for i in 0..MOVE.len().max(JOB.len()) {
        text.push(help_row(MOVE.get(i), JOB.get(i)));
    }
    text.push(Line::from(""));
    text.push(help_headings("CHANGES THINGS", theme::DANGER, "SERVER & VIEW", theme::ACCENT));
    for i in 0..MUTATE.len().max(SERVER.len()) {
        text.push(help_row(MUTATE.get(i), SERVER.get(i)));
    }
    text.push(Line::from(""));
    text.push(Line::from(vec![
        Span::styled("  \u{25cf} ", Style::default().fg(theme::DANGER)),
        Span::styled("failed   ", Style::default().fg(theme::MUTED)),
        Span::styled("\u{25d0} ", Style::default().fg(theme::WARNING)),
        Span::styled("building   ", Style::default().fg(theme::MUTED)),
        Span::styled("\u{25cf} ", Style::default().fg(theme::SUCCESS)),
        Span::styled("passed   ", Style::default().fg(theme::MUTED)),
        Span::styled("\u{23f8} ", Style::default().fg(theme::WARNING)),
        Span::styled("paused   ", Style::default().fg(theme::MUTED)),
        Span::styled("\u{2605} ", Style::default().fg(theme::FAVORITE)),
        Span::styled("favorite", Style::default().fg(theme::MUTED)),
    ]));
    text.push(Line::from(Span::styled(
        "  mouse: click focuses a pane, wheel scrolls it. q/ctrl-c quits.",
        Style::default().fg(theme::MUTED),
    )));
    text.push(Line::from(Span::styled(
        "  any key closes this",
        Style::default().fg(theme::MUTED),
    )));

    let block = styled_block("Keys", theme::ACCENT);
    f.render_widget(Paragraph::new(text).block(block), rect);
}

const HELP_KEY_W: usize = 9;
const HELP_DESC_W: usize = 19;

fn help_headings(left: &str, lc: Color, right: &str, rc: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<width$}", left, width = HELP_KEY_W + HELP_DESC_W),
            Style::default().fg(lc).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            right.to_string(),
            Style::default().fg(rc).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn help_row(left: Option<&Hint>, right: Option<&Hint>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (idx, cell) in [left, right].iter().enumerate() {
        match cell {
            Some((key, desc, mutates)) => {
                let color = if *mutates { theme::DANGER } else { theme::ACCENT };
                spans.push(Span::styled(
                    format!("{:<width$}", key, width = HELP_KEY_W),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
                // Last column stays unpadded so trailing blanks don't widen the box.
                let text = if idx == 1 {
                    (*desc).to_string()
                } else {
                    format!("{:<width$}", desc, width = HELP_DESC_W)
                };
                spans.push(Span::styled(text, Style::default().fg(theme::MUTED)));
            }
            None if idx == 0 => spans.push(Span::raw(" ".repeat(HELP_KEY_W + HELP_DESC_W))),
            None => {}
        }
    }
    Line::from(spans)
}

fn draw_confirm(f: &mut Frame, area: Rect, message: &str) {
    let rect = centered_rect(50, 20, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Confirm", theme::WARNING);
    // Multi-line messages split into Lines; a \n inside a single Line renders
    // literally. Line one is the target (pipeline and run), emphasised so you
    // always see what you are about to act on before the verb.
    let mut text: Vec<Line> = message
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(l.to_string())
            }
        })
        .collect();
    text.push(Line::from(""));
    text.push(Line::from(Span::styled(
        "y/enter confirm   n/esc cancel",
        Style::default().fg(theme::MUTED),
    )));
    f.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        rect,
    );
}

/// 'T' trigger-with-variables: entered NAME=VALUE pairs stack up as summary
/// lines, an empty entry flips to the confirm step.
fn draw_trigger_vars(f: &mut Frame, area: Rect, form: &TriggerVarsForm) {
    let rect = centered_rect(60, 55, area);
    f.render_widget(Clear, rect);
    let block = styled_block(
        format!("Trigger {} with variables", form.pipeline),
        theme::ACCENT,
    );

    let mut lines: Vec<Line> = Vec::new();
    for (name, value) in &form.vars {
        lines.push(summary_line(name, value));
    }
    if !form.vars.is_empty() {
        lines.push(Line::from(""));
    }
    if form.confirming {
        lines.push(Line::from(format!(
            "Trigger a new run of '{}' with {} variable(s)?",
            form.pipeline,
            form.vars.len()
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "y/enter trigger   n/esc cancel   backspace edit",
            Style::default().fg(theme::MUTED),
        )));
    } else {
        lines.push(field_label("Environment variable (NAME=VALUE)"));
        lines.push(Line::from(Span::styled(
            "enter adds another; an empty entry finishes",
            Style::default().fg(theme::MUTED),
        )));
        lines.push(Line::from(""));
        lines.push(input_line(&form.input, false));
        if let Some(err) = &form.error {
            lines.push(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(theme::DANGER),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "enter add/finish   esc cancel",
            Style::default().fg(theme::MUTED),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// Returns the content viewport height so scroll-up clamping can use it.
fn draw_console_log(f: &mut Frame, area: Rect, state: &ConsoleLogState, tick: u64) -> u16 {
    let rect = centered_rect(90, 88, area);
    f.render_widget(Clear, rect);
    let block = styled_block(&state.title, theme::ACCENT);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let (tabs_area, content_area, status_area, hint_area) =
        (layout[0], layout[1], layout[2], layout[3]);

    // Tab bar
    let mut tab_spans = Vec::new();
    for (i, (tab, label)) in [
        (JobTab::Console, "1 Console"),
        (JobTab::Artifacts, "2 Artifacts"),
        (JobTab::Materials, "3 Materials"),
    ]
    .iter()
    .enumerate()
    {
        if i > 0 {
            tab_spans.push(Span::styled("  |  ", Style::default().fg(theme::MUTED)));
        }
        if *tab == state.tab {
            tab_spans.push(Span::styled(
                (*label).to_string(),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            tab_spans.push(Span::styled(
                (*label).to_string(),
                Style::default().fg(theme::MUTED),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), tabs_area);

    match state.tab {
        JobTab::Console => draw_console_tab(f, content_area, state, tick),
        JobTab::Artifacts => draw_artifacts_tab(f, content_area, state, tick),
        JobTab::Materials => {
            let text: Vec<Line> = state
                .materials
                .iter()
                .map(|l| Line::from(l.clone()))
                .collect();
            let visible = content_area.height as usize;
            let scroll = state
                .scroll
                .min(state.materials.len().saturating_sub(visible));
            f.render_widget(
                Paragraph::new(text).scroll((scroll as u16, 0)),
                content_area,
            );
        }
    }

    // Status row
    let mut status_spans = Vec::new();
    if let Some(res) = &state.result {
        status_spans.push(Span::styled(
            res.clone(),
            Style::default()
                .fg(status_color(res))
                .add_modifier(Modifier::BOLD),
        ));
        status_spans.push(Span::raw("  "));
    }
    status_spans.push(Span::styled(
        format!("{} lines", state.lines.len()),
        Style::default().fg(theme::MUTED),
    ));
    if state.search_active || !state.search.is_empty() {
        status_spans.push(Span::raw("  "));
        let cursor = if state.search_active { "\u{2588}" } else { "" };
        status_spans.push(Span::styled(
            format!("/{}{}", state.search, cursor),
            Style::default().fg(theme::ACCENT),
        ));
        if !state.search.is_empty() {
            let pos = if state.matches.is_empty() {
                0
            } else {
                state.match_idx + 1
            };
            status_spans.push(Span::styled(
                format!("  {}/{} matches", pos, state.matches.len()),
                Style::default().fg(if state.matches.is_empty() {
                    theme::DANGER
                } else {
                    theme::MUTED
                }),
            ));
        }
    }
    if state.loading || state.artifacts_loading {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(
            format!("{} refreshing", spinner(tick)),
            Style::default().fg(theme::MUTED),
        ));
    }
    if state.following && state.tab == JobTab::Console {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(
            "following",
            Style::default().fg(theme::SUCCESS),
        ));
    }
    if let Some(err) = &state.error {
        status_spans.push(Span::raw("  "));
        status_spans.push(Span::styled(
            format!("! {err}"),
            Style::default().fg(theme::DANGER),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(status_spans)), status_area);

    // 'e' was reachable only from the help modal, so nobody would find it.
    let hint = match state.tab {
        JobTab::Console => {
            "tab/1-3 switch   / search   n/N match   j/k scroll   e editor   O gocd   r refresh   q/esc close"
        }
        JobTab::Artifacts => {
            "tab/1-3 switch   j/k select   enter open/close   o browser   O gocd   y copy url   q/esc close"
        }
        JobTab::Materials => "tab/1-3 switch   j/k scroll   e editor   O gocd   q/esc close",
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::MUTED),
        ))),
        hint_area,
    );
    content_area.height
}

fn draw_console_tab(f: &mut Frame, content_area: Rect, state: &ConsoleLogState, tick: u64) {
    if state.lines.is_empty() && state.loading {
        f.render_widget(
            Paragraph::new(format!("{} Loading console log...", spinner(tick))),
            content_area,
        );
        return;
    }
    if state.lines.is_empty() {
        let msg = state.error.as_deref().unwrap_or("No output yet");
        f.render_widget(
            Paragraph::new(Span::styled(
                msg.to_string(),
                Style::default().fg(theme::MUTED),
            )),
            content_area,
        );
        return;
    }
    let visible = content_area.height as usize;
    let max_scroll = state.lines.len().saturating_sub(visible);
    let scroll = state.scroll.min(max_scroll);
    let query = (!state.search.is_empty()).then(|| state.search.to_lowercase());
    // Style only the visible window: coloring all N thousand lines every frame is wasted work.
    let text: Vec<Line> = state
        .lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, l)| {
            console_line(
                l,
                query.as_deref(),
                state.matches.get(state.match_idx) == Some(&i),
            )
        })
        .collect();
    f.render_widget(Paragraph::new(text), content_area);
}

fn draw_artifacts_tab(f: &mut Frame, content_area: Rect, state: &ConsoleLogState, tick: u64) {
    let Some(rows) = &state.artifacts else {
        f.render_widget(
            Paragraph::new(format!("{} Loading artifacts...", spinner(tick))),
            content_area,
        );
        return;
    };
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No artifacts",
                Style::default().fg(theme::MUTED),
            )),
            content_area,
        );
        return;
    }
    let visible = content_area.height as usize;
    // Keep the selected row in view.
    let offset = state
        .artifact_selected
        .saturating_sub(visible.saturating_sub(1));
    let text: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, row)| {
            // Closed and open folders must look different, or there is no way to
            // tell an empty folder from an unopened one.
            let icon = match (row.is_folder, row.expanded) {
                (true, true) => "\u{25be} ",
                (true, false) => "\u{25b8} ",
                _ => "  ",
            };
            let selected = i == state.artifact_selected;
            let style = if selected {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else if row.is_folder {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(if selected { "\u{25b6} " } else { "  " }.to_string()),
                Span::raw("  ".repeat(row.depth)),
                Span::styled(format!("{icon}{}", row.name), style),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(text), content_area);
}

/// One console line -> styled spans: GoCD marker classified, timestamp muted,
/// severity keywords colored, search matches reversed.
fn console_line<'a>(raw: &'a str, query: Option<&str>, current_match: bool) -> Line<'a> {
    let (marker, rest) = if raw.len() > 3 && raw.as_bytes()[2] == b'|' {
        (&raw[..2], &raw[3..])
    } else {
        ("", raw)
    };

    // "HH:MM:SS.mmm " timestamp prefix, rendered muted. Byte 13 must be a char
    // boundary: a timestamp is pure ASCII, so a multibyte char there means the
    // sniff was a false positive (e.g. "12:34:56 \u{65e5}\u{672c}...") - skip splitting.
    let (ts, body) = if rest.len() >= 13
        && rest.as_bytes().get(2) == Some(&b':')
        && rest.as_bytes().get(5) == Some(&b':')
        && rest.is_char_boundary(13)
    {
        rest.split_at(13)
    } else {
        ("", rest)
    };

    let body_style = console_body_style(marker, body);
    let mut spans = vec![Span::styled(ts, Style::default().fg(theme::MUTED))];

    if let Some(q) = query {
        let mut pos = 0;
        for (start, end) in find_matches_ci(body, q) {
            spans.push(Span::styled(&body[pos..start], body_style));
            let hl = if current_match {
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::WARNING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Black).bg(theme::ACCENT)
            };
            spans.push(Span::styled(&body[start..end], hl));
            pos = end;
        }
        spans.push(Span::styled(&body[pos..], body_style));
    } else {
        spans.push(Span::styled(body, body_style));
    }
    Line::from(spans)
}

/// Case-insensitive, non-overlapping matches of `query` in `haystack`, as byte
/// ranges that are always valid char boundaries of the ORIGINAL string. Never
/// indexes via to_lowercase(), whose byte offsets can drift (e.g. '\u{130}').
fn find_matches_ci(haystack: &str, query: &str) -> Vec<(usize, usize)> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut iter = haystack.char_indices().peekable();
    while let Some(&(start, _)) = iter.peek() {
        let mut probe = iter.clone();
        let mut qi = 0;
        let mut end = start;
        while qi < q.len() {
            let Some((i, c)) = probe.next() else { break };
            let mut ok = true;
            for lc in c.to_lowercase() {
                if qi >= q.len() || q[qi] != lc {
                    ok = false;
                    break;
                }
                qi += 1;
            }
            if !ok {
                qi = usize::MAX;
                break;
            }
            end = i + c.len_utf8();
        }
        if qi == q.len() {
            out.push((start, end));
            // Skip past the match so highlights don't overlap.
            while iter.peek().is_some_and(|&(i, _)| i < end) {
                iter.next();
            }
        } else {
            iter.next();
        }
    }
    out
}

fn console_body_style(marker: &str, body: &str) -> Style {
    let lower = body.to_ascii_lowercase();
    // Failure signals win, then warnings, then success, then framing chatter.
    if lower.contains("error")
        || lower.contains("failed")
        || lower.contains("failure")
        || lower.contains("exception")
        || lower.contains("fatal")
        || lower.contains("traceback")
        || lower.contains("exit code: 1")
        || marker == "!!"
    {
        Style::default().fg(theme::DANGER)
    } else if lower.contains("warn") || lower.contains("deprecat") || marker == "&2" {
        Style::default().fg(theme::WARNING)
    } else if lower.contains("passed")
        || lower.contains("success")
        || lower.contains("job completed")
    {
        Style::default().fg(theme::SUCCESS)
    } else if body.starts_with("[go]") || marker == "##" {
        Style::default().fg(theme::MUTED)
    } else {
        Style::default()
    }
}

fn draw_save_view(f: &mut Frame, area: Rect, input: &str, count: usize) {
    let rect = centered_rect(55, 35, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Save view", theme::ACCENT);
    let lines = vec![
        field_label("Save current filter matches as a GoCD view"),
        Line::from(Span::styled(
            format!("{count} pipelines will be in this view (shows in the web UI too)"),
            Style::default().fg(theme::MUTED),
        )),
        Line::from(""),
        input_line(input, false),
        Line::from(""),
        Line::from(Span::styled(
            "enter save   esc cancel",
            Style::default().fg(theme::MUTED),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_view_picker(f: &mut Frame, area: Rect, app: &App, selected: usize) {
    let rect = centered_rect(50, 55, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Views", theme::ACCENT);

    let mut lines = vec![field_label("Personalized dashboard views"), Line::from("")];
    lines.push(choice_line("All pipelines", selected == 0));
    for (i, v) in app.views.iter().enumerate() {
        let label = format!("{} ({})", v.name, v.pipelines.len());
        lines.push(choice_line(&label, selected == i + 1));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "j/k select   enter apply   esc close",
        Style::default().fg(theme::MUTED),
    )));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_github_connect(f: &mut Frame, area: Rect, input: &str) {
    let rect = centered_rect(60, 45, area);
    f.render_widget(Clear, rect);
    let block = styled_block("Connect GitHub", theme::ACCENT);

    let lines = vec![
        field_label("GitHub personal access token"),
        Line::from(Span::styled(
            "optional - leave blank to use `gh auth token` automatically if you have the GitHub CLI",
            Style::default().fg(theme::MUTED),
        )),
        Line::from(""),
        input_line(input, true),
        Line::from(""),
        Line::from(Span::styled(
            "enter save   esc cancel",
            Style::default().fg(theme::MUTED),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
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
    if matches!(
        form.step,
        ReauthStep::Username | ReauthStep::Secret
    ) {
        let method = if form.use_token {
            "Access token"
        } else {
            "Username & password"
        };
        lines.push(summary_line("Method", method));
        wrote_summary = true;
    }
    if form.step == ReauthStep::Secret
        && !form.use_token
        && !form.username.is_empty()
    {
        lines.push(summary_line("User", &form.username));
        wrote_summary = true;
    }
    if wrote_summary {
        lines.push(Line::from(""));
    }

    match form.step {
        ReauthStep::ServerUrl => {
            lines.push(field_label("GoCD server URL"));
            lines.push(Line::from(Span::styled(
                "e.g. https://gocd.example.com/go",
                Style::default().fg(theme::MUTED),
            )));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, false));
        }
        ReauthStep::ChooseAuthMethod => {
            lines.push(field_label("Authenticate with"));
            lines.push(Line::from(""));
            lines.push(choice_line("Username & password", form.choice_index == 0));
            lines.push(choice_line(
                "Access token (recommended)",
                form.choice_index == 1,
            ));
        }
        ReauthStep::Username => {
            lines.push(field_label("Username"));
            lines.push(Line::from(Span::styled(
                "leave blank if the server has no auth",
                Style::default().fg(theme::MUTED),
            )));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, false));
        }
        ReauthStep::Secret => {
            lines.push(field_label(if form.use_token {
                "Access token"
            } else {
                "Password"
            }));
            lines.push(Line::from(""));
            lines.push(input_line(&form.input, true));
        }
    }

    lines.push(Line::from(""));
    let hint = if form.step.is_choice() {
        "\u{2191}/\u{2193} or j/k select   enter confirm   esc cancel"
    } else {
        "enter confirm   esc cancel"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme::MUTED),
    )));

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn summary_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("\u{2713} ", Style::default().fg(theme::SUCCESS)),
        Span::styled(format!("{label}: "), Style::default().fg(theme::MUTED)),
        Span::raw(value.to_string()),
    ])
}

fn field_label(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn input_line(input: &str, masked: bool) -> Line<'static> {
    let shown = if masked {
        "\u{2022}".repeat(input.chars().count())
    } else {
        input.to_string()
    };
    Line::from(Span::styled(
        format!("> {shown}"),
        Style::default().fg(theme::ACCENT),
    ))
}

fn choice_line(label: &str, selected: bool) -> Line<'static> {
    if selected {
        Line::from(Span::styled(
            format!("\u{25b6} {label}"),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().fg(theme::MUTED),
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    // P0 regression: byte 13 lands inside a multibyte char after a false-positive
    // timestamp sniff; must render, not panic.
    #[test]
    fn console_line_multibyte_after_timestamp_sniff() {
        let _ = console_line(
            "12:34:56 \u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8} more text",
            None,
            false,
        );
        let _ = console_line("##|10:00:00.000 caf\u{e9} ok", None, false);
    }

    // P0 regression: to_lowercase changes byte length for U+0130; highlight ranges
    // must stay valid boundaries of the original string.
    #[test]
    fn find_matches_ci_lowercase_length_drift() {
        let body = "\u{130}\u{130}\u{130}\u{130}\u{130}error";
        let ranges = find_matches_ci(body, "e");
        assert!(!ranges.is_empty());
        for (a, b) in ranges {
            assert!(body.is_char_boundary(a) && body.is_char_boundary(b));
            let _ = &body[a..b];
        }
        let _ = console_line(body, Some("e"), true);
        let _ = console_line(body, Some("\u{130}"), false);
    }

    #[test]
    fn find_matches_ci_basic() {
        assert_eq!(
            find_matches_ci("Error and ERROR", "error"),
            vec![(0, 5), (10, 15)]
        );
        assert!(find_matches_ci("abc", "").is_empty());
        assert!(find_matches_ci("abc", "zz").is_empty());
    }

    // The URL used to eat 45 columns of the header.
    #[test]
    fn short_host_keeps_only_the_host() {
        assert_eq!(super::short_host("https://gocd.example.com/go"), "gocd.example.com");
        assert_eq!(super::short_host("http://10.0.0.4:8153/go/"), "10.0.0.4:8153");
        assert_eq!(super::short_host("gocd.example.com/go"), "gocd.example.com");
        assert_eq!(super::short_host("demo"), "demo");
    }

    #[test]
    fn short_sha_is_character_safe() {
        assert_eq!(super::short_sha("25d63f5a166e44e1b86d"), "25d63f5");
        assert_eq!(super::short_sha("abc"), "abc");
        assert_eq!(super::short_sha(""), "");
        // Never panics on non-ASCII, which byte slicing would.
        assert_eq!(super::short_sha("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}").chars().count(), 7);
    }

    // The whole point of the colouring is that a failure is obvious, so a
    // regression that dropped ERROR into the default style would be silent.
    #[test]
    fn console_severity_classification() {
        let danger = super::console_body_style("", "ERROR failed to publish artifact");
        let warn = super::console_body_style("", "WARN deprecated flag");
        let pass = super::console_body_style("", "All 214 tests passed.");
        let plain = super::console_body_style("", "compiling module 3");
        assert_ne!(danger, plain, "errors must not render as plain text");
        assert_ne!(warn, plain, "warnings must not render as plain text");
        assert_ne!(pass, plain, "passes must not render as plain text");
        assert_ne!(danger, warn);
        assert_ne!(danger, pass);

        // GoCD's own stream markers carry severity even without keywords.
        assert_eq!(super::console_body_style("!!", "anything"), danger);
        assert_eq!(super::console_body_style("&2", "anything"), warn);

        // Case-insensitive: real logs are inconsistent.
        assert_eq!(super::console_body_style("", "error: boom"), danger);
        assert_eq!(super::console_body_style("", "Exception in thread"), danger);
    }

    #[test]
    fn status_glyph_and_colour_agree_on_each_state() {
        // Building and Cancelled must be visually distinct from done states,
        // since the dashboard counts lean on them.
        assert_ne!(super::dot_for("Building"), super::dot_for("Passed"));
        assert_ne!(super::dot_for("Cancelled"), super::dot_for("Passed"));
        assert_eq!(super::dot_for("Passed"), super::dot_for("Failed"), "same glyph, colour differs");
        assert_ne!(super::status_color("Passed"), super::status_color("Failed"));
        assert_ne!(super::status_color("Building"), super::status_color("Passed"));
        // Anything unrecognised falls back rather than panicking.
        assert_eq!(super::dot_for("SomethingNew"), super::dot_for(""));
    }
}
