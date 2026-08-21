# Changelog

## v0.4.0

- Personalized dashboard views: `v` opens a picker of your GoCD views
  (server-side `viewName` filtering, same as the web UI's tabs), with the
  active view shown in the header
- Create views from the TUI: filter with `/`, then `V` saves the matches
  as a new whitelist view on the server (visible in the web UI too);
  writes use `If-Match` so concurrent web edits fail loudly, never silently
- CHANGELOG.md introduced

## v0.3.0

- `R` reruns a failed run (`y` failed jobs only, `a` whole stage)
- `T` triggers with environment variables
- History pagination: older runs load as you reach the bottom; background
  refreshes keep the paginated tail
- Every git material checked and shown, not just the first
- GitHub Enterprise support via `github_api_base`
- Desktop notifications when a favorited pipeline starts failing
- Homebrew bottle: `brew install` pours a prebuilt binary in seconds

## v0.2.0

- Incremental console tailing (`?startLineNumber`) and ETag/304 dashboard
  polls: near-zero steady-state network cost
- `y` copies commit SHA / pipeline name / artifact URL via OSC 52
- `--version`, `--help`, `--config-dir`; `$XDG_CONFIG_HOME` respected
- Built-in `completions <shell>` and `man` subcommands
- CI on pushes and PRs; Windows job in the release pipeline

## v0.1.4

- Bug bash: three UTF-8 panics fixed (console rendering, search
  highlighting, materials), reconnect state leak, global ctrl-c,
  materials scroll, favorites prefetch

## v0.1.3

- Tabbed job view: Console / Artifacts / Materials
- Console log search with inline highlighting and n/N match jumping
- Severity-colored logs (failures red, warnings yellow, passes green,
  agent chatter dimmed)

## v0.1.2

- Run counter shown alongside the label so timer re-runs of one commit
  stay distinguishable

## v0.1.1

- `o` opens the run's commit on GitHub (or the pending diff when behind)
- GitHub token picked up from `gh auth token` automatically

## v0.1.0

- Initial release: three-pane dashboard, trigger/pause/unpause/cancel,
  live console logs with tail-follow, fuzzy filter, favorites, mouse,
  GitHub stale-deploy check, instant startup from a disk cache
