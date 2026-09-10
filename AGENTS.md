# lazygocd — project instructions

A terminal UI for GoCD, written in Rust with ratatui. The binary, the docs site,
and the Homebrew formula are released together.

## The docs site lives in `site/` and is committed. Always.

`site/` is the **only** source of truth for https://lazygocd.vercel.app.

- **Never** deploy from a scratchpad, a temp directory, or a copy reconstructed
  by downloading the live pages. Deploy from `site/` in this repo.
- **Always** commit site changes in the same pass that deploys them. An
  uncommitted site edit is a site edit that will be lost.
- Before deploying, confirm these still exist in the directory you are
  deploying, because they are invisible in a browser and easy to destroy:
  - `robots.txt`
  - `sitemap.xml`
  - `google3afbede6088646ea.html` (Search Console verification)
  - `vercel.json`
- `vercel.json` must keep `"cleanUrls": false` with explicit `rewrites` for each
  page. Switching to `cleanUrls: true` makes Vercel 308-redirect
  `google3afbede6088646ea.html` to an extensionless path, which breaks Search
  Console verification. Add a rewrite entry for every new page instead.
- Add every new page to `sitemap.xml`.

This rule exists because a session rebuilt the site by curling the live pages,
deployed that copy, and silently destroyed `robots.txt`, `sitemap.xml`, the
Search Console file, and the `nosniff` header.

## Release flow

**A release is not done until it has gone out EVERYWHERE.** A feature that only
exists on `main` is invisible: `brew upgrade` reports "already installed" and the
user hits `error: unexpected argument`. Never stop at a commit. Every release
must reach all six channels, because skipping one leaves it serving a stale
version:

1. git tag + `main`
2. GitHub release with both macOS tarballs and `.sha256` files
3. Homebrew tap formula (url + source sha256)
4. Homebrew bottle, so installs pour in seconds instead of compiling
5. crates.io (`cargo publish`)
6. Docs site, then deploy from `site/`

Releases are cut by hand; there is no one-shot script.

1. Bump `version` in `Cargo.toml`, add a `CHANGELOG.md` entry with the date.
2. `cargo build --release` and `cargo build --release --target x86_64-apple-darwin`.
3. Commit, then fast-forward `main` (a local hook blocks pushes naming `main`,
   so push a temp branch and move the ref with
   `gh api -X PATCH repos/Sahilll15/lazygocd/git/refs/heads/main -f sha=<sha>`).
4. Tag, push the tag, `gh release create` with both tarballs plus `.sha256`.
5. Bump `Formula/lazygocd.rb` in `Sahilll15/homebrew-tap` (url + source sha256).
6. Build and publish a bottle so users pour a binary instead of compiling:
   `brew install --build-bottle`, then `brew bottle --json --root-url=<release url>`,
   then upload the tarball **renamed from `lazygocd--X.Y.Z...` to
   `lazygocd-X.Y.Z...`** (brew writes a double dash but fetches a single one),
   then add the `bottle do` block to the formula.
7. Update the docs site and deploy from `site/`.

## The docs site is not just the changelog

A changelog entry records that something happened; it does not teach anyone to
use the feature. For every release, update what the change actually touches:

| Changed | Update |
|---|---|
| a keybinding | `keybindings.html` **and** the in-app help and footer hints |
| a config option | the config sample in `quickstart.html` |
| an install path | `install.html` |
| any user-visible capability | the prose in `features.html` |
| behaviour a page already describes | **fix that page** |
| anything at all | `changelog.html` |

The second-to-last row is the one that bites: shipping a feature can make
existing documentation *false*, not merely incomplete. v0.7.0 made the GitHub
check work on deploy pipelines while `troubleshooting.html` still told readers
deploy pipelines legitimately show nothing, so the site contradicted the binary
for several releases. Before deploying, grep for claims about what changed:

```
grep -ril "<the behaviour you changed>" site/
```

Then verify what is actually live rather than trusting your memory of updating it:

```
curl -sS https://lazygocd.vercel.app/changelog | grep -c "v<version>"
```

## Git identity

This is a personal repo. Verify before the first commit in a fresh clone:

```
git config --local user.name "Sahil Chalke"
git config --local user.email "chalke.sahil1015@gmail.com"
```

`gh` needs `env -u GITHUB_TOKEN` on **every** invocation touching this repo, and
`gh auth switch --user Sahilll15` first; the work token otherwise wins and 403s.
Switch back to `SahilCs15` when done.

## API constraints worth remembering

- The bare `/api/dashboard` response is **already filtered** to the user's
  Default personalized view. Client-side filtering by a saved view's pipeline
  list therefore matches nothing. Views must be applied server-side with
  `?viewName=`.
- `/api/internal/pipeline_selection` is an internal, uncontracted endpoint. A
  GoCD maintainer explicitly asked that it not be depended on. It currently
  backs the `v`/`V` view features and should be removed.
- Console logs accept `?startLineNumber=N` (0-based) for incremental tailing.
- `/api/dashboard` honours `If-None-Match` and answers 304.
- Error bodies are not always JSON. A proxy in front of GoCD returns HTML, so
  never echo a response body into the UI unsanitised.

## Style

- Comments: 2 lines maximum, and only for a non-obvious constraint or trap.
- Test against a real instance before claiming a feature works. The live
  instance is several GoCD versions behind head; say so rather than implying
  broad version coverage.

## Never put real instance data in fixtures, fixtures included

This is a public personal repo, developed against an employer's GoCD instance.
Real pipeline names, group names, hostnames and ticket IDs must not appear in
source, tests, docs, screenshots, or recordings. Test fixtures are the easy
miss: a `sanitize()` test shipped a real pipeline name to crates.io for three
releases because a fixture wanted a realistic-looking string.

- Use invented names: `web-app`, `api-build-test`, `ghe.corp.io`, `example.com`.
- crates.io versions are immutable. A name that ships cannot be unshipped, only
  yanked, so catch it before `cargo publish`.
- Internal recordings live outside the repo (`~/Desktop/lazygocd-internal-demo`),
  never in `assets/` or `site/`.
- Audit command. `$(git rev-list --all)` does NOT word-split in zsh, so the
  obvious one-liner passes all SHAs as a single argument, git errors to stderr,
  and the sweep reports a false clean. Pipe through xargs:

```
git rev-list --all | xargs git grep -I -i -E '<employer>|<gocd host>|<env prefix>'
```

  For binaries and every blob ever written, not just text in the current tree:

```
git rev-list --objects --all | awk 'NF>1{print $1}' | sort -u | while read -r o; do
  [ "$(git cat-file -t "$o")" = blob ] || continue
  git cat-file blob "$o" | LC_ALL=C grep -aqiE '<pattern>' && echo "HIT $o"
done
```
