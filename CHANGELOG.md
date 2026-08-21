# Changelog

## v0.5.0

Interface pass. The tool had grown from six keybindings to fourteen without
the layout being revisited.

- Footer shows only what the focused pane can do (six hints, labelled with the
  pane) instead of fourteen hints that ran off the edge of the screen. Keys
  that change server state render red so a destructive action never hides
  among navigation keys.
- Header is one line: host only instead of the full URL, with the fleet counts
  promoted into it. The row below is now dedicated to status and errors, so a
  message gets the full width instead of being appended to the header and cut
  off mid-sentence.
- `?` help is grouped into four task blocks across two columns (16 lines,
  down from 35) with a legend for the status glyphs, which were previously
  undocumented.
- Confirm dialogs lead with the target (pipeline name and run number) on its
  own emphasised line, then the action.
- Views: pressing `v` after a failed fetch now reports the failure instead of
  claiming the server has no views. `g`/`G` jump to the ends of the picker.
- Error text no longer echoes raw HTML when a proxy or gateway answers instead
  of GoCD, and 403 / timeout errors suggest a likely cause.

## Unreleased

- Snyk CI integration added: generates a CycloneDX SBOM for Cargo
  dependencies and scans it with `snyk sbom test` when `SNYK_TOKEN` is
  configured

## v0.4.1 - 2026-08-21

- Release verification against a live GoCD 23.5.0 server: creating a
  temporary personalized view, loading it through server-side `viewName`
  filtering, and removing it again all passed
- Release workflow fixed for current GitHub-hosted macOS Intel runners
  (`macos-15-intel` instead of the retired `macos-13` image)
- Rust sources formatted with `cargo fmt`; unused personalized-view helper
  removed

## v0.4.0 - 2026-08-21

- Personalized dashboard views: `v` opens a picker of your GoCD views
  (server-side `viewName` filtering, same as the web UI's tabs), with the
  active view shown in the header
- Create views from the TUI: filter with `/`, then `V` saves the matches
  as a new whitelist view on the server (visible in the web UI too);
  writes use `If-Match` so concurrent web edits fail loudly, never silently
- CHANGELOG.md introduced with the full release history to date

## v0.3.0 - 2026-08-21

- `R` reruns a failed run (`y` failed jobs only, `a` whole stage)
- `T` triggers with environment variables
- History pagination: older runs load as you reach the bottom; background
  refreshes keep the paginated tail
- Every git material checked and shown, not just the first
- GitHub Enterprise support via `github_api_base`
- Desktop notifications when a favorited pipeline starts failing
- Homebrew bottle: `brew install` pours a prebuilt binary in seconds

## v0.2.0 - 2026-08-21

- Incremental console tailing (`?startLineNumber`) and ETag/304 dashboard
  polls: near-zero steady-state network cost
- `y` copies commit SHA / pipeline name / artifact URL via OSC 52
- `--version`, `--help`, `--config-dir`; `$XDG_CONFIG_HOME` respected
- Built-in `completions <shell>` and `man` subcommands
- CI on pushes and PRs; Windows job in the release pipeline

## v0.1.4 - 2026-08-21

- Bug bash: three UTF-8 panics fixed (console rendering, search
  highlighting, materials), reconnect state leak, global ctrl-c,
  materials scroll, favorites prefetch

## v0.1.3 - 2026-08-21

- Tabbed job view: Console / Artifacts / Materials
- Console log search with inline highlighting and n/N match jumping
- Severity-colored logs (failures red, warnings yellow, passes green,
  agent chatter dimmed)

## v0.1.2 - 2026-08-21

- Run counter shown alongside the label so timer re-runs of one commit
  stay distinguishable

## v0.1.1 - 2026-08-21

- `o` opens the run's commit on GitHub (or the pending diff when behind)
- GitHub token picked up from `gh auth token` automatically

## v0.1.0 - 2026-08-21

- Initial release: three-pane dashboard, trigger/pause/unpause/cancel,
  live console logs with tail-follow, fuzzy filter, favorites, mouse,
  GitHub stale-deploy check, instant startup from a disk cache
