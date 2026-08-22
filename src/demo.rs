//! Fictional GoCD data for `lazygocd --demo`, so the interface can be tried
//! without a server or credentials.
//!
//! Everything here is returned as JSON and parsed by the same serde models the
//! real API path uses, which keeps the demo honest: if a response shape changes,
//! the demo breaks too rather than drifting quietly out of date.

/// Every pipeline in the demo, as (group, name, latest status, paused).
const PIPELINES: &[(&str, &str, &str, bool)] = &[
    ("frontend", "web-app-build", "Passed", false),
    ("frontend", "web-app-deploy-staging", "Passed", false),
    ("frontend", "web-app-deploy-prod", "Building", false),
    ("frontend", "design-system-publish", "Passed", false),
    ("backend", "api-build-test", "Failed", false),
    ("backend", "api-deploy-staging", "Passed", false),
    ("backend", "api-deploy-prod", "Passed", true),
    ("backend", "worker-service-build", "Passed", false),
    ("backend", "search-service-build", "Cancelled", false),
    ("infrastructure", "terraform-plan", "Passed", false),
    ("infrastructure", "terraform-apply", "Passed", true),
    ("infrastructure", "docker-base-images", "Passed", false),
    ("mobile", "ios-app-build", "Passed", false),
    ("mobile", "android-app-build", "Failed", false),
];

const AUTHORS: &[&str] = &["alex", "priya", "sam", "jordan"];

const COMMITS: &[(&str, &str)] = &[
    ("9f2c1ab", "Fix flaky retry logic in the upload path"),
    ("4e8d773", "Bump dependencies and tighten CSP"),
    ("c31a9e0", "Add dark mode to the settings page"),
    ("7b45f12", "Cache invalidation for the search index"),
    ("e90bc44", "Speed up cold starts by lazy-loading plugins"),
    ("21d7a8f", "Refactor pagination out of the API client"),
    ("88e3c5d", "Handle unicode in webhook payloads"),
    ("5a1f9b7", "Retry transient S3 errors on artifact upload"),
];

/// Stable pseudo-random index so the same pipeline always looks the same
/// across redraws and restarts.
fn seed(name: &str) -> usize {
    name.bytes().fold(7usize, |acc, b| acc.wrapping_mul(31).wrapping_add(b as usize))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const HOUR_MS: i64 = 3_600_000;

pub fn dashboard_json(view: Option<&str>) -> String {
    let allowed: Option<Vec<&str>> = match view {
        Some("frontend only") => Some(vec!["frontend"]),
        Some("prod only") => Some(vec![]), // filtered by name below
        _ => None,
    };

    let mut groups: Vec<String> = Vec::new();
    let mut pipelines: Vec<String> = Vec::new();

    for (group, _, _, _) in PIPELINES {
        if groups.iter().any(|g| g.contains(&format!("\"name\": \"{group}\""))) {
            continue;
        }
        if let Some(list) = &allowed
            && !list.is_empty()
            && !list.contains(group)
        {
            continue;
        }
        let members: Vec<String> = PIPELINES
            .iter()
            .filter(|(g, n, _, _)| {
                g == group && (view != Some("prod only") || n.contains("prod"))
            })
            .map(|(_, n, _, _)| format!("\"{n}\""))
            .collect();
        if members.is_empty() {
            continue;
        }
        groups.push(format!(
            "{{\"name\": \"{group}\", \"pipelines\": [{}]}}",
            members.join(", ")
        ));
    }

    for (group, name, status, paused) in PIPELINES {
        if let Some(list) = &allowed
            && !list.is_empty()
            && !list.contains(group)
        {
            continue;
        }
        if view == Some("prod only") && !name.contains("prod") {
            continue;
        }
        let s = seed(name);
        let (sha, _) = COMMITS[s % COMMITS.len()];
        let author = AUTHORS[s % AUTHORS.len()];
        let pause_block = if *paused {
            "{\"paused\": true, \"paused_by\": \"priya\", \"pause_reason\": \"release freeze\"}"
        } else {
            "{\"paused\": false}"
        };
        pipelines.push(format!(
            "{{\"name\": \"{name}\", \"locked\": false, \"pause_info\": {pause_block}, \
             \"can_pause\": true, \"can_operate\": true, \"_embedded\": {{\"instances\": [\
             {{\"label\": \"{sha}\", \"counter\": {counter}, \
             \"triggered_by\": \"changes by {author}\", \
             \"_embedded\": {{\"stages\": [{{\"name\": \"build\", \"status\": \"{status}\"}}]}}}}]}}}}",
            counter = 40 + (s % 60),
        ));
    }

    format!(
        "{{\"_embedded\": {{\"pipeline_groups\": [{}], \"pipelines\": [{}]}}}}",
        groups.join(", "),
        pipelines.join(", ")
    )
}

pub fn history_json(pipeline: &str, after: Option<u64>) -> String {
    let s = seed(pipeline);
    let latest = PIPELINES
        .iter()
        .find(|(_, n, _, _)| *n == pipeline)
        .map(|(_, _, st, _)| *st)
        .unwrap_or("Passed");
    let newest = 40 + (s % 60) as i64;
    let start = after.map(|a| a as i64 - 1).unwrap_or(newest);
    let now = now_ms();

    let mut runs = Vec::new();
    for i in 0..8 {
        let counter = start - i;
        if counter < 1 {
            break;
        }
        let (sha, msg) = COMMITS[(s + i as usize) % COMMITS.len()];
        let author = AUTHORS[(s + i as usize) % AUTHORS.len()];
        let status = if counter == newest {
            latest
        } else if i % 3 == 0 {
            "Failed"
        } else {
            "Passed"
        };
        let scheduled = now - (newest - counter) * 7 * HOUR_MS;
        let second_stage = if pipeline.contains("deploy") { "deploy" } else { "test" };
        runs.push(format!(
            "{{\"name\": \"{pipeline}\", \"counter\": {counter}, \"label\": \"{sha}\", \
             \"scheduled_date\": {scheduled}, \
             \"build_cause\": {{\"trigger_message\": \"modified by {author} <{author}@example.com>\", \
             \"approver\": \"changes\", \"material_revisions\": [\
             {{\"material\": {{\"type\": \"Git\", \"description\": \"URL: git@github.com:acme/{pipeline}.git, Branch: main\"}}, \
             \"modifications\": [{{\"revision\": \"{sha}{pad}\", \
             \"user_name\": \"{author} <{author}@example.com>\", \"comment\": \"{msg}\", \
             \"modified_time\": {committed}}}]}}]}}, \
             \"stages\": [\
             {{\"name\": \"build\", \"status\": \"{status}\", \"approval_type\": \"success\", \
             \"scheduled_date\": {scheduled}, \"counter\": \"1\", \
             \"jobs\": [{{\"name\": \"compile\", \"result\": \"{status}\", \"state\": \"Completed\"}}, \
             {{\"name\": \"unit-tests\", \"result\": \"{status}\", \"state\": \"Completed\"}}]}}, \
             {{\"name\": \"{second_stage}\", \"status\": \"{status}\", \"approval_type\": \"manual\", \
             \"scheduled_date\": {later}, \"counter\": \"1\", \
             \"jobs\": [{{\"name\": \"run\", \"result\": \"{status}\", \"state\": \"Completed\"}}]}}]}}",
            pad = "0".repeat(33),
            committed = scheduled - 2 * HOUR_MS,
            later = scheduled + HOUR_MS / 4,
        ));
    }

    let next = start - 8;
    let links = if next > 1 {
        format!(
            ",\"_links\": {{\"next\": {{\"href\": \"https://demo.invalid/api/pipelines/{pipeline}/history?after={next}\"}}}}"
        )
    } else {
        String::new()
    };
    format!("{{\"pipelines\": [{}]{}}}", runs.join(", "), links)
}

/// A log with the marker prefixes, severities, and unicode that the colouring
/// and search paths need to handle.
pub fn console_log(start_line: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..38 {
        lines.push(format!(
            "##|10:{:02}:{:02}.000 [go] Task: step {i}: compiling module {i}... ok ({}ms)",
            i % 60,
            (i * 7) % 60,
            i * 37
        ));
    }
    lines.push("&2|10:38:02.100 WARN  deprecated flag --legacy-resolver will be removed".into());
    lines.push("##|10:38:44.000 [go] Running 214 tests across 12 suites".into());
    lines.push("##|10:39:01.000 café-service: 完了 (unicode is fine here)".into());
    lines.push("!!|10:39:12.480 ERROR failed to publish artifact: connection reset".into());
    lines.push("##|10:39:13.000 [go] Retrying upload (attempt 2 of 3)... ok".into());
    lines.push("##|10:39:58.900 [go] All 214 tests passed.".into());
    lines.push("##|10:40:00.120 [go] Uploading artifacts... done".into());
    lines.push("##|10:40:01.000 [go] Job completed: build/1/compile (exit code: 0)".into());
    lines
        .into_iter()
        .skip(start_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn artifacts_json() -> &'static str {
    r#"[
      {"name": "cruise-output", "type": "folder", "files": [
        {"name": "console.log", "type": "file", "url": "https://demo.invalid/files/console.log"},
        {"name": "md5.checksum", "type": "file", "url": "https://demo.invalid/files/md5.checksum"}
      ]},
      {"name": "dist", "type": "folder", "files": [
        {"name": "app.tar.gz", "type": "file", "url": "https://demo.invalid/files/app.tar.gz"},
        {"name": "sourcemaps", "type": "folder", "files": [
          {"name": "app.js.map", "type": "file", "url": "https://demo.invalid/files/app.js.map"}
        ]}
      ]},
      {"name": "test-reports", "type": "folder", "files": [
        {"name": "junit.xml", "type": "file", "url": "https://demo.invalid/files/junit.xml"},
        {"name": "coverage.html", "type": "file", "url": "https://demo.invalid/files/coverage.html"}
      ]}
    ]"#
}

pub fn views_json() -> &'static str {
    r#"{"filters": [
      {"name": "Default", "type": "blacklist", "state": [], "pipelines": []},
      {"name": "frontend only", "type": "whitelist", "state": [],
       "pipelines": ["web-app-build", "web-app-deploy-staging", "web-app-deploy-prod", "design-system-publish"]},
      {"name": "prod only", "type": "whitelist", "state": [],
       "pipelines": ["web-app-deploy-prod", "api-deploy-prod"]}
    ]}"#
}

#[cfg(test)]
mod tests {
    use crate::model::{ArtifactNode, DashboardResponse, HistoryResponse, ViewFilters};

    // The demo data has to parse with the production models, otherwise --demo
    // silently drifts away from the real response shapes.
    #[test]
    fn demo_payloads_parse_with_the_real_models() {
        let d: DashboardResponse = serde_json::from_str(&super::dashboard_json(None)).unwrap();
        assert_eq!(d.embedded.pipeline_groups.len(), 4);
        assert_eq!(d.embedded.pipelines.len(), 14);

        let h: HistoryResponse = serde_json::from_str(&super::history_json("api-build-test", None)).unwrap();
        assert!(!h.pipelines.is_empty());
        assert!(h.pipelines[0].stages.len() == 2);

        let a: Vec<ArtifactNode> = serde_json::from_str(super::artifacts_json()).unwrap();
        assert_eq!(a.len(), 3);

        let v: ViewFilters = serde_json::from_str(super::views_json()).unwrap();
        assert_eq!(v.filters.len(), 3);
    }

    #[test]
    fn views_filter_the_demo_dashboard() {
        let all: DashboardResponse = serde_json::from_str(&super::dashboard_json(None)).unwrap();
        let front: DashboardResponse =
            serde_json::from_str(&super::dashboard_json(Some("frontend only"))).unwrap();
        assert!(front.embedded.pipelines.len() < all.embedded.pipelines.len());
        assert_eq!(front.embedded.pipeline_groups.len(), 1);
    }

    #[test]
    fn console_log_supports_incremental_tailing() {
        let full = super::console_log(0);
        let tail = super::console_log(40);
        assert!(full.lines().count() > tail.lines().count());
        assert!(full.contains("ERROR failed to publish"));
    }
}
