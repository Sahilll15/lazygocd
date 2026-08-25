# Changelog

## v0.10.4 - 2026-08-25

- The connect form no longer asks about TLS. Certificates are always verified,
  and the form ends at the credential. The old question defaulted to the safe
  answer but put the decision in front of people at the one moment they had no
  information to make it with, and its only hint was "only for self-signed
  certs", which never said what saying yes costs.
- A rejected certificate now says what to do about it instead of echoing
  reqwest's wording: add the server's CA to your trust store, or set
  `insecure_skip_verify = true` in `config.toml`. The message row grows to two
  lines when an error is too long for the terminal width, so the half naming the
  setting is no longer the half that gets cut off.
- `insecure_skip_verify` is now config-only and is never rewritten by
  reconnecting, so a value you set by hand survives pressing `A`.

## v0.10.3 - 2026-08-25

Security hardening. No feature or keybinding changed.

- Logs opened with `e` are written to a private per-user directory under the
  system temp dir (mode `0700`, file mode `0600`) instead of loose in a shared
  `/tmp` at a predictable path. A console log routinely contains build secrets,
  so on a multi-user Linux box any other local user could read one, and could
  pre-create a symlink at that path to redirect the write.
- `dashboard_cache.json` and `favorites.json` are written mode `0600`, and the
  config directory is created mode `0700`. The cache lists every pipeline and
  group name on the server and was world-readable.
- `config.toml` is created owner-only rather than written and then chmodded,
  which left the credential readable for the moment in between. An existing
  file at looser permissions is tightened on the next write.
- Host, owner and repo parsed out of a GoCD Git material are validated before
  they reach a URL, and the browser handoff rejects anything unexpected.
  Windows opens URLs through `cmd /C start`, which re-parses `&`, `|` and `^`
  that Rust's argument quoting leaves alone, so a crafted material URL could
  run a command when you pressed `o`.
- Pipeline, stage, job and branch names are percent-encoded into request paths
  rather than interpolated raw.
- The GitHub token is only sent over `https`, never to a plain-http API base.
- Docs now state that skipping TLS verification sends your credential over a
  connection nothing authenticates.
- CI: the Snyk token reaches only the scan step, not the steps that run
  crates.io build scripts, and the release workflow no longer interpolates a
  ref name straight into a shell command.

## v0.10.2 - 2026-08-25

- No behaviour change. The source, tests, and documentation no longer carry
  details of the private GoCD instance this was developed against: a test
  fixture used a real pipeline name, and the docs quoted exact group and
  pipeline counts. The measurements those numbers supported are unchanged
  (5.3 MB dashboard payload down to 165 KB gzipped, ~20s cold load down to
  2 to 4s, on GoCD 23.5.0).

## v0.10.1 - 2026-08-25

- The Details pane scrolls. It rendered without a scroll offset, so a run with
  several stages and jobs had everything past the pane height clipped and
  unreachable: the selection cursor moved off-screen invisibly and `enter`
  opened a log you could not see you had picked. The pane now follows the
  selected stage or job in both directions.

## v0.10.0 - 2026-08-22

- The Artifacts tab is a real tree now. Every folder was permanently expanded,
  because the API tree was flattened once at fetch time and the structure thrown
  away, so a build with a deep artifact tree filled the pane with rows you could
  not collapse. Folders now start closed: `enter` opens or closes one, `l`/right
  opens, `h`/left closes and steps out to the parent, and an open folder shows
  ▾ against a closed ▸.
- A node carrying children counts as a folder even when GoCD omits the type
  field, which previously left its children unreachable.
- Folder state is keyed by full path, so two folders with the same name under
  different parents no longer open together.

## v0.9.0 - 2026-08-22

- `editor` setting in config.toml, so `e` no longer depends on the shell. Many
  machines set neither `$VISUAL` nor `$EDITOR`, and lazygocd would silently fall
  back to the `less` pager. Precedence is now config, then `$VISUAL`, then
  `$EDITOR`, then `less` or `vi`. A blank value falls through rather than
  shadowing the environment.

  ```toml
  editor = "code --wait"   # or "nvim", "zed --wait", "subl --wait"
  ```

## v0.8.1 - 2026-08-22

- Fixed `e` with GUI editors. VS Code and Zed return as soon as the window has
  the file (about four seconds for a cold VS Code launch), so deleting the temp
  file when the command exited pulled it out from under the open tab. The file
  is now left in place and stale ones are swept at startup instead.

## v0.8.0 - 2026-08-22

- `e` opens the current log in your own editor. lazygocd writes the buffer to a
  temp file, suspends the terminal properly (mouse capture off, alternate screen
  released), runs `$VISUAL` or `$EDITOR`, then restores and cleans up. Inline
  editors like vim and nvim take over the terminal; GUI editors like code and
  zed open beside it and the TUI returns immediately. Works on the Console and
  Materials tabs.

## v0.7.0 - 2026-08-22

- The GitHub deployed-commit check now works on deploy pipelines. A deploy run
  usually has no Git material of its own: its only input is a Pipeline material
  whose revision reads `upstream-name/389/stage/1`, so the commit lives one hop
  away. The check now walks that dependency chain (depth-capped at four hops,
  cycle-guarded) and reports the commit it finds, labelled `via <pipeline>
  #<counter>` so it is clear the commit was inherited rather than direct.

## v0.6.1 - 2026-08-22

Demo mode was not as isolated as v0.6.0 claimed.

- `--demo` read the real favorites file, so actual pipeline names appeared in
  a mode intended for screenshots and screen sharing. It now seeds fictional
  favorites and never reads that file.
- `--demo` could also write to the real config directory. Pressing `f` would
  overwrite favorites.json with demo names, a dashboard refresh would overwrite
  the cache, and completing the `A` or `@` forms would rewrite config.toml. All
  four writes are now suppressed in demo mode.
- Regression test asserts every demo favorite is one of the fixture pipelines,
  so a real name reappearing fails the build.

## v0.6.0 - 2026-08-22

- `--demo` launches the interface with fictional pipelines: no server, no
  credentials, and nothing written to disk. Fixtures are parsed by the same
  serde models the live API path uses, so the demo cannot drift silently out
  of sync with real response shapes.

## v0.5.0 - 2026-08-22

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
- Snyk CI integration: generates a CycloneDX SBOM for Cargo dependencies and
  scans it with `snyk sbom test` when `SNYK_TOKEN` is configured.
- Published to crates.io, so `cargo install lazygocd` works without pointing
  cargo at the git repository.

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
