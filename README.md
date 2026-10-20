# Local review

A read-only, live Git diff reviewer. One Rust binary embeds the web UI; no Node runtime, frontend build, or CDN. Git is required. Pushed GitHub PR reviews additionally require the authenticated GitHub CLI (`gh`).

## Download

Download a prebuilt archive from [the latest release](https://github.com/rhart92/review/releases/latest):

| Platform | Archive suffix |
| --- | --- |
| macOS Apple Silicon | `macos-arm64.tar.gz` |
| macOS Intel | `macos-x86_64.tar.gz` |
| Linux x86-64 (glibc 2.35+) | `linux-x86_64.tar.gz` |
| Linux ARM64 (glibc 2.39+) | `linux-arm64.tar.gz` |

Extract the archive, then run the included binary:

```sh
tar -xzf review-v0.1.0-macos-arm64.tar.gz
cd review-v0.1.0-macos-arm64
./review --repo /path/to/repo
```

Substitute your platform's archive name. No Rust installation is needed. Git must
be installed; pushed PR reviews also need authenticated `gh`. Downloads require
repository access while this repository is private.

Each release includes `SHA256SUMS`. From the directory containing the downloaded
archive and checksum file, verify it with
`shasum -a 256 --ignore-missing -c SHA256SUMS` on macOS or
`sha256sum --ignore-missing -c SHA256SUMS` on Linux.

Binaries are unsigned and macOS builds are not notarized. macOS may block the
first launch; after verifying the download, use **System Settings → Privacy &
Security → Open Anyway** if prompted. Keep the server private; see **Boundaries**.

## Run

```sh
cargo build --release
./target/release/review --repo /path/to/repo
```

Open **http://127.0.0.1:4173/**. The binary can be copied anywhere; source files and web assets are not needed at runtime.

```sh
review --repo /path/to/repo --base main --bind 127.0.0.1:4173
# Or run anywhere and choose a repository in the browser:
review
```

`--base` accepts a local ref, remote-tracking ref (such as `origin/main`), or commit. It defaults to `main`; there is no automatic fetch. The comparison is **that commit → current working tree**, not a merge-base/three-dot comparison. Committed, staged, and unstaged changes are combined into their net result. Edits that cancel out do not appear. Nonignored untracked files and explicitly staged ignored files are included. The application never stages, commits, or modifies the reviewed repository/index.

### Choose a repository in the UI

Click the repository name in the top bar to open the folder picker. Browse folders, use **Up** / **Home**, or enter a path (including `~/...`). Git repositories and linked worktrees are marked **Git**. Selecting a subfolder uses its containing repository. Set **Local base ref** to `main`, `master`, or another available ref, then click **Use repository**.

Without `--repo`, the app uses the current working tree if there is one; otherwise it starts with the picker open. CLI options still supply defaults. Switching repositories from an existing review opens local changes; enter a PR number afterward to review a pushed PR. An open comment draft must be saved or canceled before switching. Saved comments are retained separately for each review.

The URL records the selection in **`repo`**, **`base`**, and **`pr`** query parameters, so reloads, bookmarks, and separate tabs retain their own repository without changing other tabs:

```text
http://127.0.0.1:4173/?repo=%2Fpath%2Fto%2Frepo&base=main&pr=0
http://127.0.0.1:4173/?repo=%2Fpath%2Fto%2Frepo&base=main&pr=123
```

Paths refer to the filesystem of the machine **running the binary**, not the browser's machine. This also applies when accessing the app through Caddy; the picker does not upload a folder.

### Review a pushed GitHub PR

Enter its number in **PR # → Review PR**, or start directly:

```sh
gh auth login
review --repo /path/to/repo --pr 123
```

PR numbers are resolved against the checkout's GitHub repository using `gh`. Bookmark `/?pr=123` (or `/review/?pr=123` behind Caddy). **Local changes** switches back to the working tree; `?pr=0` explicitly selects local mode even when the server was started with `--pr`.

PR mode compares the pushed PR head against its merge-base with the PR's base commit, like GitHub. It does **not** include unpushed commits or local edits. GitHub resolves the exact merge-base; the app reuses local Git objects and fetches only missing, shallow, blob-filtered snapshots into an application-owned temporary bare repository. Missing changed-file blobs are fetched together, rather than downloading the repository's complete history or every file. Full-context expansion remains available. The reviewed repository's index, refs, and files are not changed. Refresh observes new pushes; missing `gh`, authentication/network failures, and unavailable commits produce explicit errors.

## Review

- All changed files load together in one scrollable review. **Refresh all** reloads every file in one review request.
- The header below the toolbar shows visible-file totals and **+added / −removed** line counts. Each file shows its own counts both in its header and beside its sidebar filename. Both **Hide test files** and the path filter update the overall totals. Unchanged context and binary contents do not contribute textual line counts; expanding context does not change totals.
- Click a **file heading** to copy its full repo-relative path. Sidebar filenames both jump to the file and copy its path without fetching it again. Clipboard failures offer selectable text instead.
- The panel icon next to **Local review** hides/restores the file sidebar. Drag its right-hand divider to resize it. Width and collapsed state are remembered in this browser; collapsing/restoring preserves the width and neither action discards an open comment. Width is bounded so the diff remains usable in narrow windows. Sidebar text is not selectable while dragging; the search input still supports normal editing.
- Focus the divider to resize with **Left/Right** (10 pixels), **Shift + Left/Right** (50 pixels), or **Home/End** (minimum/maximum). **Escape** cancels an active drag without saving its new width.
- Code is syntax-highlighted with bundled Prism: GitHub-like colors in light mode and **Rosé Pine** colors in dark mode. JSX and TSX use dedicated grammars, keeping component names, prop names, strings, numbers, and embedded JavaScript expressions distinct. Nested scopes retain their own colors rather than inheriting the enclosing tag color. Common code/config languages are recognized by filename; unsupported files remain plain text. Highlighting is applied separately to old/new code and retains multiline context within each visible hunk. There are no CDN requests or additional runtime dependencies.
- **Select code text to copy source only.** Line numbers and diff `+`/`−` markers are nonselectable decorations, and clipboard output excludes headers, hunk controls, and other review UI. Partial selections, indentation, blank lines, and literal `+`/`-` characters in code are preserved. Ordinary code clicks/double-clicks do not open a comment editor.
- **Drag anywhere in the line-number gutter**, including blank columns and `+`/`−` markers, to start a range comment forward or backward. Empty old/new columns use the line's available side instead of starting native text selection. Release to open the editor, or press **Escape** to cancel. Holding near the diff's top/bottom edge scrolls while selecting. Alternatively, click a line number (or focus a diff row and press Enter), then **Shift-click another line to extend the comment**. Comment gestures suppress native text selection; comment textareas remain normally editable.
- Save with the button or **⌘/Ctrl + Enter**. Comment bodies can contain multiple paragraphs. Edit/delete inline or from **Comments**.
- **Clear comments** in the top bar opens an in-app confirmation dialog; choose **Clear comments** again to remove every saved comment for the current review, including hidden-file comments and the open draft. No native browser popup is required. It does not affect other PRs/repositories or local-review scopes. **Cancel** or **Escape** leaves comments and the draft untouched. Storage failures appear in the dialog and leave existing comments intact.
- **Copy feedback** exports every saved comment with file, line/range (`file:20-23`), old-side marker, all captured lines, and comment body. PR feedback also includes its URL. A nonempty open draft is saved when copying. If clipboard access is unavailable, a dialog offers selectable text.
- GitHub-style **↑ / ↓** gutter controls share a slim, tinted row with each hunk's range label, distinct from the file header. Hover for **Expand above / Expand below** tooltips, or focus and press Enter. Each reveals up to 20 nearby lines in that direction without expanding unrelated hunks. Full context is fetched once per file version and then expanded locally. **Whole file** shows up to 1,000,000 context lines and is hidden when all content is already visible; exact file line totals distinguish EOF from still-hidden context.
- **Hide test files** filters conventional test/spec filenames and test directories from both the sidebar and diff. It does not remove their comments from exported feedback. The preference is remembered in this browser; the path filter also applies to both views.
- **Live** refreshes the whole review while the tab is visible: local checks every 2.5 seconds when idle; PR checks every 30 seconds. Code text selections, comment-range drags, and open editors pause background updates so the diff cannot move underneath your interaction. Clear/finish the selection to resume; **Refresh all** explicitly clears a code selection before reloading.

Comments are stored in this browser's local storage, scoped by repository path and base-ref name, plus PR URL in PR mode. Existing single-line comments remain readable; local and PR feedback stay separate. Comments survive reloads, not clearing browser data. Different origins/ports and browsers have separate comments; there is no server-side comment database or multi-tab synchronization. Export before changing how you access the app.

Anchors are deliberately not rebased. If the line number, text, or base commit no longer matches, the comment stays available under its original anchor or in **Comments**, including when its file disappears. Feedback always includes the original captured content.

## Reverse proxy with Caddy

The UI uses relative asset/API URLs, so it works at `/review/` when Caddy strips that prefix.

Explicitly allow the private site's hostname (no scheme or port; repeat the option for additional hosts):

```sh
review --repo /path/to/repo --allow-host review.local
```

Only `localhost` and loopback IP hosts are allowed by default. Other Host headers, foreign Origin headers, and cross-site browser API requests receive HTTP 403. Forwarded-host headers are not trusted; the proxy must preserve the allowed hostname or send a loopback Host header.

### Your own host-running Caddy

Add these directives to your existing site, ahead of its catch-all proxy:

```caddyfile
redir /review /review/ 308
handle_path /review/* {
    reverse_proxy 127.0.0.1:4173
}
```

Keep your existing application handler in a separate catch-all `handle { ... }`. Visit the site's `/review/` URL, including the trailing slash. The review process must remain running. Use a private development site, not a publicly accessible origin.

A separate trusted origin is safest. A `/review/` path is not an isolation boundary: scripts in other applications sharing the origin can access review APIs and browser-stored comments.

## Boundaries

- **No authentication.** Loopback is the default. Binding to `0.0.0.0` exposes repository diffs and server-side folder/repository browsing to reachable clients; use only a trusted machine/network, firewall the port, and do not publicly proxy it. The Caddy admin API must also stay private.
- **Trusted local repositories only.** This is not a sandbox for malicious Git configuration. Hooks, external diffs, text conversion, and fsmonitor are disabled, but Git may still invoke locally configured clean/process filters or credential helpers. PR content is read from isolated Git objects without checking it out. Filesystem checks do not protect against a malicious local process racing file changes.
- Host validation and browser security headers reduce browser-origin attacks; they are not authentication or a limit on what an authorized local client can read. The repository picker intentionally browses server-local directories. Do not add untrusted or attacker-controlled proxy hostnames.
- Binary changes get Git's binary notice, not a binary viewer. Renames appear as deletion/addition. Submodules get Git's textual summary, not a nested review.
- Raw Git mode headers such as `new file mode 100644` are hidden. Mode-only changes remain listed with a metadata-only notice.
- Filenames must be UTF-8; unsupported names produce an explicit error. Non-UTF-8 text content is displayed with replacement characters. Symlink contents are diffed, not target contents; paths through symlinked parent directories are rejected.
- Reads are live, not atomic across requests. A file changed/deleted during a request may require Refresh. Huge files/full-context diffs can be expensive to render.
- Local reviews use a private base index, seeded with the real index's filesystem-stat cache so unchanged files do not need to be rehashed across the entire checkout. Refreshing that cache writes only the private index; user assume-unchanged/skip-worktree flags do not hide review changes. Each repository/base retains its private index across UI switches until the base commit changes. Live reviews still scan for changed and untracked files, so large repositories are not instantaneous. Periodic polling does not queue behind an active request. PR mode caches its isolated snapshots until the PR head/base changes and batches missing changed-file blobs; GitHub metadata is still checked on refresh.

## Development

```sh
cargo test
cargo run -- --repo /path/to/repo
```

Assets are embedded at compile time; rebuild after changing `web/`. Prism 1.30.0 is vendored under its MIT license at `web/vendor/prism.LICENSE` (also served at `/vendor/prism.LICENSE`).

Real-Git regression tests cover combined whole-review patches, canceled index edits, restored staged deletions, ignored additions, live base changes, unusual names, binary files, symlink/path safety, exact old/new file line totals, shallow/partial PR snapshots, changed-blob loading, linked worktrees without modifying the target index or refs, folder browsing, optional startup repositories, and independent repository/base selections.

### Publishing a release

Update the package version in `Cargo.toml` and `Cargo.lock`, commit it, then push
a matching `v<version>` tag. `.github/workflows/release.yml` tests, builds, and
smoke-checks all four native binaries before publishing the archives and SHA-256
checksums to a GitHub release. The tag must match the package version.

## License

MIT licensed; see [LICENSE](LICENSE). Bundled Prism retains its
[MIT license notice](web/vendor/prism.LICENSE).
