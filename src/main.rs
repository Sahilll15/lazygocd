mod api;
mod app;
mod config;
mod demo;
mod editor;
mod github;
mod inspect;
mod model;
mod notify;
mod ui;

use app::App;
use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use std::path::PathBuf;
use std::time::Duration;

const LONG_ABOUT: &str = "A fast, keyboard-driven terminal UI for GoCD pipelines. Browse \
pipeline groups, trigger and monitor runs, drill into stages and jobs, tail console logs, and \
open builds in the browser without leaving the terminal. Connection details come from \
config.toml, the in-app setup prompt, or GOCD_* environment variables. \
Pass --demo to explore the interface with fictional data and no server.\n\n\
Naming a pipeline prints its latest run and exits instead of opening the UI: \
`lazygocd web-app`. The name is matched loosely, so a unique fragment or the initials \
are enough, and an ambiguous query lists what it matched. Add --json for scripting.";

#[derive(Parser)]
#[command(name = "lazygocd", version, about, long_about = LONG_ABOUT)]
struct Cli {
    /// Directory for config.toml and cached state (overrides $XDG_CONFIG_HOME/lazygocd)
    #[arg(long, value_name = "PATH")]
    config_dir: Option<PathBuf>,

    /// Explore the interface with fictional data: no server, no credentials, no config written
    #[arg(long)]
    demo: bool,

    /// Print this pipeline's latest run and exit, without opening the UI.
    /// Matched loosely: a unique fragment or the initials will do
    #[arg(value_name = "PIPELINE")]
    pipeline: Option<String>,

    /// With PIPELINE, print machine-readable JSON instead of a summary
    #[arg(long, requires = "pipeline", conflicts_with = "logs")]
    json: bool,

    /// With PIPELINE, how many recent runs (or ambiguous matches) to list
    #[arg(short = 'n', long, value_name = "N", default_value_t = 10, requires = "pipeline")]
    limit: usize,

    /// With PIPELINE, print the console log instead of a summary. Picks the
    /// running job, else the failed one, unless --stage/--job say otherwise
    #[arg(short = 'l', long, requires = "pipeline")]
    logs: bool,

    /// With --logs, keep tailing until the stage finishes
    #[arg(short = 'f', long, requires = "logs")]
    follow: bool,

    /// With PIPELINE, list recent runs only, without the stage and job detail
    #[arg(long, requires = "pipeline", conflicts_with_all = ["logs", "json"])]
    history: bool,

    /// With PIPELINE, target this run counter instead of the latest run
    #[arg(long, value_name = "COUNTER", requires = "pipeline")]
    run: Option<i64>,

    /// With --logs, the stage to read from
    #[arg(long, value_name = "NAME", requires = "logs")]
    stage: Option<String>,

    /// With --logs, the job to read from
    #[arg(long, value_name = "NAME", requires = "logs")]
    job: Option<String>,

    /// With --follow, seconds between polls (GoCD has no push for console logs)
    #[arg(long, value_name = "SECS", default_value_t = 3, requires = "follow")]
    interval: u64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print shell completions for the given shell to stdout
    #[command(hide = true)]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Print the man page (roff) to stdout
    #[command(hide = true)]
    Man,
    /// Print cached pipeline names, one per line, for shell completion
    #[command(hide = true)]
    Pipelines,
}

/// clap generates a static script whose PIPELINE argument falls back to file
/// completion. Rewrite that one slot to read `lazygocd pipelines` instead.
fn with_pipeline_completion(shell: clap_complete::Shell, script: String) -> String {
    use clap_complete::Shell;
    const NOTE: &str = "\n# PIPELINE completes from lazygocd's cached dashboard (no network).\n";
    match shell {
        Shell::Zsh => {
            // The positional is the only ::pipeline line; :_default on it means
            // "complete filenames", which is never right for a pipeline name.
            let patched = script.replace(
                "the initials will do:_default'",
                "the initials will do:_lazygocd_pipeline_names'",
            );
            // The autoloaded file calls _lazygocd on its last line, so the
            // action function has to be defined above that, not appended.
            let func = format!(
                "{NOTE}_lazygocd_pipeline_names() {{\n  \
                 local -a names\n  \
                 names=(${{(f)\"$(lazygocd pipelines 2>/dev/null)\"}})\n  \
                 (( ${{#names}} )) && _describe -t pipelines pipeline names\n\
                 }}\n"
            );
            match patched.find('\n') {
                Some(i) => format!("{}{func}{}", &patched[..=i], &patched[i + 1..]),
                None => format!("{patched}{func}"),
            }
        }
        // Bash has no per-argument hook, so wrap clap's function and append
        // names whenever the current word is not a flag.
        Shell::Bash => format!(
            "{script}{NOTE}_lazygocd_with_pipelines() {{\n  \
             _lazygocd\n  \
             local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  \
             if [[ $cur != -* ]]; then\n    \
             COMPREPLY+=( $(compgen -W \"$(lazygocd pipelines 2>/dev/null)\" -- \"$cur\") )\n  \
             fi\n\
             }}\n\
             complete -o bashdefault -o default -F _lazygocd_with_pipelines lazygocd\n"
        ),
        Shell::Fish => format!(
            "{script}{NOTE}complete -c lazygocd -n '__fish_use_subcommand' \
             -a '(lazygocd pipelines 2>/dev/null)' -d pipeline\n"
        ),
        _ => script,
    }
}

fn main() -> anyhow::Result<()> {
    // Parse before any terminal setup so --version/--help/subcommands never
    // enter the alternate screen.
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Completions { shell }) => {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "lazygocd", &mut script);
            let script = String::from_utf8(script).context("completion script is not UTF-8")?;
            print!("{}", with_pipeline_completion(shell, script));
            return Ok(());
        }
        Some(Command::Man) => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            return Ok(());
        }
        Some(Command::Pipelines) => {
            if let Some(dir) = cli.config_dir {
                config::set_config_dir_override(dir);
            }
            // A completion script or `| head` closes the pipe early; that is
            // normal here, not an error worth panicking over.
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            for name in app::cached_pipeline_names() {
                if writeln!(out, "{name}").is_err() {
                    break;
                }
            }
            return Ok(());
        }
        None => {}
    }

    if let Some(dir) = cli.config_dir {
        config::set_config_dir_override(dir);
    }

    // A named pipeline is a one-shot query: print and exit, never touching the
    // alternate screen, so the output stays pipeable.
    if let Some(query) = cli.pipeline {
        let client = if cli.demo {
            api::GoCdClient::demo()?
        } else {
            let cfg = match config::load() {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            if cfg.server_url.trim().is_empty() {
                eprintln!(
                    "No GoCD server configured yet. Run lazygocd with no arguments to set one up, \
                     or set GOCD_URL."
                );
                std::process::exit(1);
            }
            api::GoCdClient::new(&cfg)?
        };
        let opts = inspect::Opts {
            limit: cli.limit,
            json: cli.json,
            logs: cli.logs,
            follow: cli.follow,
            history: cli.history,
            run: cli.run,
            stage: cli.stage,
            job: cli.job,
            interval: cli.interval,
        };
        match inspect::run(&client, &query, &opts) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("{e:#}");
                std::process::exit(1);
            }
        }
    }

    // Demo mode never reads config or the cache, so it works on a fresh machine.
    let mut app = if cli.demo {
        App::demo()?
    } else {
        let cfg = match config::load() {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        App::new(&cfg)?
    };

    // Remove temp logs from previous sessions (a day old or more).
    editor::cleanup_stale(std::time::Duration::from_secs(24 * 60 * 60));


    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    // ratatui's panic hook restores the screen but not mouse capture; chain ours
    // in front so a panic doesn't leave the terminal spewing mouse escapes.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        prev_hook(info);
    }));
    let result = run(&mut terminal, &mut app);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    // Redraw on demand: draw only after input/API events (dirty) or while a
    // spinner is animating - idle CPU stays near zero at the long poll timeout.
    let mut dirty = true;
    loop {
        let animating = app.needs_animation();
        if dirty || animating {
            terminal.draw(|f| ui::draw(f, app))?;
            app.tick = app.tick.wrapping_add(1);
            dirty = false;
        }

        let timeout = if animating || app.hover_target.is_some() {
            Duration::from_millis(40)
        } else {
            Duration::from_millis(250)
        };
        // Hand the terminal to the user's editor, then take it back. Order
        // matters: mouse capture off and alternate screen left before the child
        // starts, both restored after, or the terminal is left unusable.
        if let Some(req) = app.pending_edit.take() {
            let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
            ratatui::restore();

            let outcome = editor::edit(&req);

            *terminal = ratatui::init();
            let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            terminal.clear()?;
            dirty = true;

            match outcome {
                // The file is deliberately left in place: a GUI editor returns
                // as soon as its window opens, so deleting now would pull the
                // file out from under the tab. Swept on the next start instead.
                Ok(()) => {
                    app.status_line = match editor::temp_path(&req.file_name) {
                        Ok(p) => format!("Opened in editor: {}", p.display()),
                        Err(_) => "Opened in editor".to_string(),
                    };
                }
                Err(e) => app.error_line = Some(format!("{e:#}")),
            }
            continue;
        }

        if event::poll(timeout)? {
            // Drain every queued event so a burst (held key, wheel fling)
            // coalesces into a single redraw instead of one frame per event.
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.handle_key(key);
                        dirty = true;
                    }
                    Event::Mouse(m) => dirty |= app.handle_mouse(m),
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
                if app.should_quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        while let Ok(ev) = app.rx.try_recv() {
            app.handle_api_event(ev);
            dirty = true;
        }

        app.maybe_prefetch();
        app.maybe_poll();
        app.maybe_poll_console();

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
