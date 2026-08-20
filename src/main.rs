mod api;
mod app;
mod config;
mod github;
mod model;
mod ui;

use app::App;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let cfg = match config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let mut app = App::new(&cfg)?;

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
