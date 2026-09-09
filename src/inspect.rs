//! Non-interactive pipeline lookup: `lazygocd <pipeline>` prints a run summary
//! and exits, so a status check does not need the TUI.

use crate::api::GoCdClient;
use crate::app::{fuzzy_match, gocd_pipeline_url, gocd_run_url};
use crate::model::{DashboardEmbedded, PipelineInstance, StageInstance};
use anyhow::{Context, Result};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

/// What a query narrowed down to. Ambiguity is a distinct outcome from a miss:
/// one wants a shorter query, the other a different one.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    One(String),
    Many(Vec<String>),
    None,
}

/// Match in tiers so a query that names a pipeline exactly is never drowned out
/// by the hundreds it is also a subsequence of.
pub fn resolve(names: &[String], query: &str) -> Resolved {
    let q = query.trim();
    if q.is_empty() {
        return Resolved::None;
    }

    let exact: Vec<String> = names
        .iter()
        .filter(|n| n.eq_ignore_ascii_case(q))
        .cloned()
        .collect();
    let lower = q.to_ascii_lowercase();
    let substring: Vec<String> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().contains(&lower))
        .cloned()
        .collect();
    let fuzzy: Vec<String> = names
        .iter()
        .filter(|n| fuzzy_match(n, q).is_some())
        .cloned()
        .collect();

    for tier in [exact, substring, fuzzy] {
        match tier.len() {
            0 => continue,
            1 => return Resolved::One(tier.into_iter().next().expect("len checked")),
            _ => {
                let mut sorted = tier;
                sorted.sort();
                return Resolved::Many(sorted);
            }
        }
    }
    Resolved::None
}

struct Paint(bool);

impl Paint {
    fn wrap(&self, code: &str, text: &str) -> String {
        if self.0 {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
    fn bold(&self, t: &str) -> String {
        self.wrap("1", t)
    }
    fn dim(&self, t: &str) -> String {
        self.wrap("90", t)
    }
    fn status(&self, s: &str) -> String {
        let code = match s {
            "Passed" => "32",
            "Failed" => "31",
            "Building" | "Scheduled" => "33",
            "Cancelled" => "90",
            _ => "0",
        };
        self.wrap(code, s)
    }
}

/// What the CLI was asked to show. Defaults to the run summary.
#[derive(Debug, Default)]
pub struct Opts {
    pub limit: usize,
    pub json: bool,
    pub logs: bool,
    pub follow: bool,
    pub history: bool,
    pub run: Option<i64>,
    pub stage: Option<String>,
    pub job: Option<String>,
    pub interval: u64,
}

/// Run the lookup. Returns the process exit code: 1 for a miss or an ambiguous
/// query, so a script can tell "no such pipeline" from "here is your pipeline".
pub fn run(client: &GoCdClient, query: &str, opts: &Opts) -> Result<i32> {
    let limit = opts.limit.max(1);
    let as_json = opts.json;
    let dash = client
        .fetch_dashboard(None, None)
        .context("loading the pipeline list")?
        .map(|(embedded, _)| embedded)
        .unwrap_or(DashboardEmbedded {
            pipeline_groups: Vec::new(),
            pipelines: Vec::new(),
        });

    let names: Vec<String> = dash.pipelines.iter().map(|p| p.name.clone()).collect();
    let name = match resolve(&names, query) {
        Resolved::One(n) => n,
        Resolved::Many(all) => {
            eprintln!("{} pipelines match {query:?}:", all.len());
            for n in all.iter().take(limit.max(1)) {
                eprintln!("  {n}");
            }
            if all.len() > limit.max(1) {
                eprintln!("  ... and {} more", all.len() - limit.max(1));
            }
            eprintln!("Narrow the query, or pass the full name.");
            return Ok(1);
        }
        Resolved::None => {
            eprintln!("No pipeline matches {query:?}.");
            if names.is_empty() {
                eprintln!("The dashboard returned no pipelines at all; check the server URL and token.");
            }
            return Ok(1);
        }
    };

    let group = dash
        .pipeline_groups
        .iter()
        .find(|g| g.pipelines.contains(&name))
        .map(|g| g.name.clone());
    let paused = dash
        .pipelines
        .iter()
        .find(|p| p.name == name)
        .is_some_and(|p| p.pause_info.paused);

    let (runs, _) = client
        .fetch_history_page(&name, None)
        .with_context(|| format!("loading history for {name}"))?;

    // A counter older than the first history page is still addressable directly,
    // so fall back to fetching that one instance rather than calling it missing.
    let fetched = match opts.run {
        Some(c) if !runs.iter().any(|r| r.counter == c) => {
            client.fetch_pipeline_instance(&name, c).ok()
        }
        _ => None,
    };
    let run = pick_run(&runs, opts.run).or(fetched.as_ref());
    let Some(run) = run else {
        if let Some(c) = opts.run {
            eprintln!("{name} has no run #{c}.");
            return Ok(1);
        }
        if as_json {
            print_json(&name, group.as_deref(), paused, &runs, limit)?;
        } else {
            println!("{name}");
            println!("No runs yet.");
        }
        return Ok(0);
    };

    if opts.logs {
        return stream_logs(client, &name, run, opts);
    }
    if as_json {
        print_json(&name, group.as_deref(), paused, &runs, limit)?;
    } else if opts.history {
        print_history(&name, group.as_deref(), &runs, limit);
    } else {
        print_human(client, &name, group.as_deref(), paused, &runs, limit);
    }
    Ok(0)
}

fn pick_run(runs: &[PipelineInstance], counter: Option<i64>) -> Option<&PipelineInstance> {
    match counter {
        Some(c) => runs.iter().find(|r| r.counter == c),
        None => runs.first(),
    }
}

/// GoCD sets result to "Unknown" until a job finishes, so the live word is in
/// `state`. Showing "Unknown" for a job that is plainly Building is just wrong.
fn job_label(job: &crate::model::JobInstance) -> &str {
    match job.result.as_deref() {
        Some("Unknown") | None => job.state.as_deref().unwrap_or("-"),
        Some(r) => r,
    }
}

/// A job is worth watching if it has not reached a terminal result yet. GoCD
/// reports in-flight work through `state`, and only sets `result` at the end.
fn is_running(job: &crate::model::JobInstance) -> bool {
    !matches!(
        job.result.as_deref(),
        Some("Passed") | Some("Failed") | Some("Cancelled")
    )
}

/// Which job to tail when the user did not say. A running job is what someone
/// watching a live pipeline means; a failed one is what they mean afterwards.
fn pick_job(run: &PipelineInstance) -> Option<(&StageInstance, &crate::model::JobInstance)> {
    let pairs = || run.stages.iter().flat_map(|s| s.jobs.iter().map(move |j| (s, j)));
    pairs()
        .find(|(s, j)| s.is_active() && is_running(j))
        .or_else(|| pairs().find(|(_, j)| j.result.as_deref() == Some("Failed")))
        .or_else(|| pairs().last())
}

fn find_job<'a>(
    run: &'a PipelineInstance,
    stage: Option<&str>,
    job: Option<&str>,
) -> Result<(&'a StageInstance, &'a crate::model::JobInstance), String> {
    if stage.is_none() && job.is_none() {
        return pick_job(run).ok_or_else(|| format!("Run #{} has no jobs yet.", run.counter));
    }
    let stages: Vec<&StageInstance> = match stage {
        Some(want) => run
            .stages
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(want))
            .collect(),
        None => run.stages.iter().collect(),
    };
    if stages.is_empty() {
        let have: Vec<&str> = run.stages.iter().map(|s| s.name.as_str()).collect();
        return Err(format!(
            "Run #{} has no stage {:?}. Stages: {}",
            run.counter,
            stage.unwrap_or(""),
            have.join(", ")
        ));
    }
    let mut hits = stages.iter().flat_map(|s| {
        s.jobs
            .iter()
            .filter(|j| job.is_none_or(|w| j.name.eq_ignore_ascii_case(w)))
            .map(move |j| (*s, j))
    });
    let first = hits.next();
    match first {
        Some(hit) => Ok(hit),
        None => {
            let have: Vec<&str> = stages
                .iter()
                .flat_map(|s| s.jobs.iter().map(|j| j.name.as_str()))
                .collect();
            Err(format!(
                "No job {:?} in run #{}. Jobs: {}",
                job.unwrap_or(""),
                run.counter,
                have.join(", ")
            ))
        }
    }
}

/// Print the job's console log, then keep polling for more while --follow is on
/// and its stage is still active, the same 3s cadence the TUI tails at.
fn stream_logs(
    client: &GoCdClient,
    name: &str,
    run: &PipelineInstance,
    opts: &Opts,
) -> Result<i32> {
    let (stage, job) = match find_job(run, opts.stage.as_deref(), opts.job.as_deref()) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("{msg}");
            return Ok(1);
        }
    };
    let Some(stage_counter) = stage.counter.clone() else {
        eprintln!(
            "Stage {} of run #{} has not been scheduled, so it has no console log yet.",
            stage.name, run.counter
        );
        return Ok(1);
    };
    let (stage_name, job_name) = (stage.name.clone(), job.name.clone());
    let counter = run.counter;

    // The header goes to stderr so the log itself stays clean for a pipe.
    let p = Paint(std::io::stderr().is_terminal());
    eprintln!(
        "{} {} #{counter} {}/{}  {}",
        p.dim("==>"),
        p.bold(name),
        stage_name,
        job_name,
        p.status(job_label(job))
    );

    let mut sent = 0usize;
    let paint_out = Paint(std::io::stdout().is_terminal());
    let mut out = std::io::stdout();
    loop {
        let chunk = client.fetch_console_log(
            name,
            counter,
            &stage_name,
            &stage_counter,
            &job_name,
            sent,
        );
        let mut grew = false;
        match chunk {
            Ok(text) if !text.is_empty() => {
                grew = true;
                sent += text.lines().count();
                for raw in text.lines() {
                    writeln!(out, "{}", log_line(&paint_out, raw))?;
                }
                out.flush()?;
            }
            Ok(_) => {}
            Err(e) => {
                // A queued job has no log file yet; that is worth waiting out
                // under --follow rather than failing.
                if sent == 0 && !opts.follow {
                    eprintln!("{e:#}");
                    return Ok(1);
                }
            }
        }

        if !opts.follow {
            return Ok(0);
        }
        // Output arriving is itself proof the job is alive, so skip the extra
        // status round trip and spend the time tailing instead.
        if !grew {
            let fresh = client
                .fetch_pipeline_instance(name, counter)
                .with_context(|| format!("refreshing {name} run {counter}"))?;
            let stage_now = fresh.stages.iter().find(|s| s.name == stage_name);
            if !stage_now.is_some_and(|s| s.is_active()) {
                let result = stage_now
                    .and_then(|s| s.status.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                eprintln!("{} {stage_name} {}", p.dim("==>"), p.status(&result));
                return Ok(i32::from(result == "Failed"));
            }
        }
        let until = Instant::now() + Duration::from_secs(opts.interval.max(1));
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn print_history(name: &str, group: Option<&str>, runs: &[PipelineInstance], limit: usize) {
    let p = Paint(std::io::stdout().is_terminal());
    let mut head = p.bold(name);
    if let Some(g) = group {
        head.push_str(&p.dim(&format!("  ({g})")));
    }
    println!("{head}\n");
    for r in runs.iter().take(limit) {
        let when = r.scheduled_date.map(local).unwrap_or_else(|| "-".to_string());
        println!(
            "  {:<8} {:<12} {:<10} {:<20} {}",
            format!("#{}", r.counter),
            r.label.chars().take(12).collect::<String>(),
            p.status(r.overall_status()),
            when,
            p.dim(
                r.build_cause
                    .as_ref()
                    .and_then(|c| c.approver.as_deref())
                    .unwrap_or("")
            )
        );
    }
}

fn print_json(
    name: &str,
    group: Option<&str>,
    paused: bool,
    runs: &[PipelineInstance],
    limit: usize,
) -> Result<()> {
    let latest = runs.first();
    let out = serde_json::json!({
        "pipeline": name,
        "group": group,
        "paused": paused,
        "status": latest.map(|r| r.overall_status()),
        "latest": latest.map(run_json),
        "runs": runs.iter().take(limit).map(run_json).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn run_json(r: &PipelineInstance) -> serde_json::Value {
    serde_json::json!({
        "counter": r.counter,
        "label": r.label,
        "status": r.overall_status(),
        "scheduled_at": r.scheduled_date.map(iso),
        "triggered_by": r.build_cause.as_ref().and_then(|c| c.approver.clone()),
        "cause": r.build_cause.as_ref().and_then(|c| c.trigger_message.clone()),
        "commit": r.git_ref().map(|g| g.deployed_sha),
        "stages": r.stages.iter().map(|s| serde_json::json!({
            "name": s.name,
            "status": s.status,
            "approval": s.approval_type,
            "jobs": s.jobs.iter().map(|j| serde_json::json!({
                "name": j.name,
                "result": job_label(j),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn print_human(
    client: &GoCdClient,
    name: &str,
    group: Option<&str>,
    paused: bool,
    runs: &[PipelineInstance],
    limit: usize,
) {
    let p = Paint(std::io::stdout().is_terminal());

    let mut head = p.bold(name);
    if let Some(g) = group {
        head.push_str(&p.dim(&format!("  ({g})")));
    }
    if paused {
        head.push_str(&p.wrap("33", "  [paused]"));
    }
    println!("{head}");

    let Some(latest) = runs.first() else {
        println!("{}", p.dim("No runs yet."));
        return;
    };

    println!(
        "\n{} #{} {}  {}",
        p.dim("Run"),
        latest.counter,
        latest.label,
        p.status(latest.overall_status())
    );
    if let Some(ts) = latest.scheduled_date {
        println!("{} {}  {}", p.dim("Started"), local(ts), p.dim(&age(ts)));
    }
    if let Some(cause) = &latest.build_cause {
        if let Some(by) = &cause.approver {
            println!("{} {by}", p.dim("By     "));
        } else if let Some(msg) = &cause.trigger_message {
            println!("{} {msg}", p.dim("Cause  "));
        }
    }
    if let Some(m) = latest.git_modification() {
        if let Some(sha) = &m.revision {
            let short: String = sha.chars().take(12).collect();
            let subject = m
                .comment
                .as_deref()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            println!("{} {short}  {subject}", p.dim("Commit "));
        }
        if let Some(author) = &m.user_name {
            println!("{} {author}", p.dim("Author "));
        }
    }

    if !latest.stages.is_empty() {
        println!();
        for stage in &latest.stages {
            let status = stage.status.as_deref().unwrap_or("Unknown");
            let approval = match stage.approval_type.as_deref() {
                Some(a) => p.dim(&format!("  ({a})")),
                None => String::new(),
            };
            println!("  {:<28} {}{}", stage.name, p.status(status), approval);
            for job in &stage.jobs {
                println!("    {:<26} {}", job.name, p.status(job_label(job)));
            }
        }
    }

    let rest: Vec<&PipelineInstance> = runs.iter().take(limit).collect();
    if rest.len() > 1 {
        println!("\n{}", p.dim("Recent runs"));
        for r in rest {
            let when = r.scheduled_date.map(age).unwrap_or_else(|| "-".to_string());
            println!(
                "  {:<8} {:<10} {:<10} {}",
                format!("#{}", r.counter),
                p.status(r.overall_status()),
                when,
                p.dim(
                    r.build_cause
                        .as_ref()
                        .and_then(|c| c.approver.as_deref())
                        .unwrap_or("")
                )
            );
        }
    }

    // Skipped in demo mode, where the server URL is a placeholder.
    if let Some(base) = client.web_base() {
        let url = gocd_run_url(base, name, latest.counter)
            .or_else(|_| gocd_pipeline_url(base, name))
            .ok();
        if let Some(url) = url {
            println!("\n{} {}", p.dim("Open  "), url);
        }
    }
}

/// GoCD frames every console line as "xx|HH:MM:SS.mmm body", where xx is the
/// stream marker. The marker is protocol, not content, so it goes.
fn log_line(p: &Paint, raw: &str) -> String {
    let (marker, rest) = if raw.len() > 3 && raw.as_bytes()[2] == b'|' {
        (&raw[..2], &raw[3..])
    } else {
        ("", raw)
    };
    match severity(marker, rest) {
        Some(code) => p.wrap(code, rest),
        None => rest.to_string(),
    }
}

/// Same precedence as the TUI's log colouring: failures, then warnings, then
/// successes, then GoCD's own framing chatter.
fn severity(marker: &str, body: &str) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    if marker == "!!"
        || ["error", "failed", "failure", "exception", "fatal", "traceback", "exit code: 1"]
            .iter()
            .any(|k| lower.contains(k))
    {
        Some("31")
    } else if marker == "&2" || lower.contains("warn") || lower.contains("deprecat") {
        Some("33")
    } else if ["passed", "success", "job completed"].iter().any(|k| lower.contains(k)) {
        Some("32")
    } else if marker == "##" || body.trim_start().starts_with("[go]") {
        Some("90")
    } else {
        None
    }
}

fn local(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => "-".to_string(),
    }
}

fn iso(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(dt) => dt.to_rfc3339(),
        None => String::new(),
    }
}

fn age(ms: i64) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let secs = (now - ms).max(0) / 1000;
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
    use super::{Resolved, resolve};

    fn names() -> Vec<String> {
        [
            "web-app",
            "web-app-build",
            "web-app-deploy",
            "api-build-test",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    // An exact name is also a substring of two others here, so tiering is the
    // only thing that keeps `web-app` from being called ambiguous.
    #[test]
    fn an_exact_name_wins_over_the_pipelines_it_prefixes() {
        assert_eq!(
            resolve(&names(), "web-app"),
            Resolved::One("web-app".to_string())
        );
        assert_eq!(
            resolve(&names(), "WEB-APP"),
            Resolved::One("web-app".to_string())
        );
    }

    #[test]
    fn a_unique_substring_resolves_and_a_shared_one_lists() {
        assert_eq!(
            resolve(&names(), "deploy"),
            Resolved::One("web-app-deploy".to_string())
        );
        assert_eq!(
            resolve(&names(), "build"),
            Resolved::Many(vec![
                "api-build-test".to_string(),
                "web-app-build".to_string(),
            ])
        );
    }

    // GoCD's marker is framing, not content, and dropping it is the whole
    // reason CLI output reads like a log instead of a protocol dump.
    #[test]
    fn the_stream_marker_is_stripped_and_the_timestamp_kept() {
        let plain = super::Paint(false);
        assert_eq!(
            super::log_line(&plain, "pr|10:00:00.000 [go] Start to prepare"),
            "10:00:00.000 [go] Start to prepare"
        );
        assert_eq!(super::log_line(&plain, "no marker here"), "no marker here");
        assert_eq!(super::log_line(&plain, "ab"), "ab");
    }

    #[test]
    fn severity_follows_the_same_precedence_as_the_tui() {
        assert_eq!(super::severity("", "npm ERR! failed"), Some("31"));
        assert_eq!(super::severity("!!", "anything"), Some("31"));
        assert_eq!(super::severity("", "deprecated api"), Some("33"));
        assert_eq!(super::severity("", "all tests passed"), Some("32"));
        assert_eq!(super::severity("##", "framing"), Some("90"));
        assert_eq!(super::severity("", "[go] chatter"), Some("90"));
        assert_eq!(super::severity("", "plain output"), None);
        // A failure keyword outranks a success keyword on the same line.
        assert_eq!(super::severity("", "1 passed, 1 failed"), Some("31"));
    }

    fn job(name: &str, result: Option<&str>, state: Option<&str>) -> crate::model::JobInstance {
        serde_json::from_value(serde_json::json!({
            "name": name, "result": result, "state": state,
        }))
        .expect("job fixture")
    }

    // A running job carries result "Unknown"; printing that instead of
    // "Building" was wrong on every live pipeline.
    #[test]
    fn a_running_job_reports_its_state_not_unknown() {
        assert_eq!(super::job_label(&job("j", Some("Unknown"), Some("Building"))), "Building");
        assert_eq!(super::job_label(&job("j", Some("Passed"), Some("Completed"))), "Passed");
        assert_eq!(super::job_label(&job("j", None, None)), "-");
    }

    fn run_with(stages: serde_json::Value) -> crate::model::PipelineInstance {
        serde_json::from_value(serde_json::json!({
            "name": "web-app", "counter": 1, "stages": stages,
        }))
        .expect("run fixture")
    }

    // Watching a live pipeline means the running job; afterwards it means the
    // one that broke. Falling back to the last job keeps a passed run useful.
    #[test]
    fn job_selection_prefers_running_then_failed_then_last() {
        let live = run_with(serde_json::json!([
            {"name": "build", "status": "Passed", "counter": "1",
             "jobs": [{"name": "compile", "result": "Passed"}]},
            {"name": "test", "status": "Building", "counter": "1",
             "jobs": [{"name": "unit", "result": "Unknown", "state": "Building"}]},
        ]));
        assert_eq!(super::pick_job(&live).map(|(_, j)| j.name.as_str()), Some("unit"));

        let broken = run_with(serde_json::json!([
            {"name": "build", "status": "Failed", "counter": "1",
             "jobs": [{"name": "compile", "result": "Passed"},
                      {"name": "lint", "result": "Failed"}]},
        ]));
        assert_eq!(super::pick_job(&broken).map(|(_, j)| j.name.as_str()), Some("lint"));

        let green = run_with(serde_json::json!([
            {"name": "build", "status": "Passed", "counter": "1",
             "jobs": [{"name": "compile", "result": "Passed"},
                      {"name": "package", "result": "Passed"}]},
        ]));
        assert_eq!(super::pick_job(&green).map(|(_, j)| j.name.as_str()), Some("package"));

        assert!(super::pick_job(&run_with(serde_json::json!([]))).is_none());
    }

    #[test]
    fn an_unknown_stage_or_job_names_what_is_available() {
        let r = run_with(serde_json::json!([
            {"name": "build", "status": "Passed", "counter": "1",
             "jobs": [{"name": "compile", "result": "Passed"}]},
        ]));
        let err = super::find_job(&r, Some("nope"), None).unwrap_err();
        assert!(err.contains("build"), "{err}");
        let err = super::find_job(&r, None, Some("nope")).unwrap_err();
        assert!(err.contains("compile"), "{err}");
        assert!(super::find_job(&r, Some("BUILD"), Some("COMPILE")).is_ok());
    }

    // Falling through to fuzzy is what makes initials work, but it must not
    // fire while any substring still matches.
    #[test]
    fn fuzzy_is_the_last_resort() {
        assert_eq!(
            resolve(&names(), "abt"),
            Resolved::One("api-build-test".to_string())
        );
        assert_eq!(resolve(&names(), "zzz"), Resolved::None);
        assert_eq!(resolve(&names(), "  "), Resolved::None);
    }
}
