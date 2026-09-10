# gitdrift — design

Date: 2026-09-10
Status: approved, pending implementation plan

## Problem

~510 git repositories live under `~/gitlab`. There is no way to see, at a
glance, which of them have drifted — uncommitted work, unpushed commits,
missed pulls, forgotten stashes. `cdg` finds repos to `cd` into but says
nothing about their state.

`gitdrift` scans a directory tree, computes the git state of every repository
it finds, and presents them in a TUI with the drifted ones first.

## Goals

- Fast enough to be reflexive at 500+ repos: first paint immediately, full
  local scan in well under a second on a warm cache.
- Drifted repos surface first, without filtering or sorting by hand.
- Selecting a repo shows enough state (branches, stashes, remotes, recent
  commits) to decide what to do about it.
- `fetch` and `pull --ff-only` runnable in place.
- Scriptable: the same scan available as a plain table or JSON.

## Non-goals

Push. Merge, rebase, cherry-pick. Creating, applying or dropping stashes.
Worktree management. Credential prompting or storage. Multi-host repo
discovery (only local filesystem).

## Interface

```
gitdrift [PATH]                 # TUI (default)
gitdrift --plain [PATH]         # aligned table to stdout, exit
gitdrift --json  [PATH]         # JSON array to stdout, exit
gitdrift --drifted-only [PATH]  # applies to every mode
gitdrift --color=auto|always|never
```

With no `PATH`, roots come from config. Multiple roots are allowed.

### Config — `~/.config/gitdrift/config.toml`

```toml
roots  = ["~/gitlab"]
prune  = [".terragrunt-cache", ".terraform", "node_modules", "target",
          ".venv", "vendor", ".cache", ".direnv"]
# editor = "nvim"          # unset: $VISUAL, then $EDITOR, then vi
fetch_concurrency = 8
```

`prune` in config **replaces** the built-in list; `prune_extra` appends to it.
Missing config file is not an error — the defaults above apply.

## Architecture

Engine and UI are separate. Nothing in `discover`/`status`/`detail`/`drift`
knows about a terminal, which is what makes `--json` nearly free and the
ordering logic testable without a pty.

| Module | Responsibility |
|---|---|
| `config` | load + merge config file, CLI flags, env |
| `cli` | argument parsing, mode dispatch |
| `discover` | roots → repository paths |
| `status` | repo path → `RepoStatus` (cheap, computed for every repo) |
| `detail` | repo path → `RepoDetail` (expensive, computed lazily) |
| `drift` | scoring and ordering |
| `actions` | fetch, pull --ff-only, shell, editor |
| `theme` | the semantic palette; the single source of colour truth |
| `ui` | ratatui app state, event handling, rendering |
| `render` | `--plain` and `--json` output |

### Dependencies

- `gix` (gitoxide) — repository inspection. Pure Rust, no libgit2 C build,
  fastest available status implementation.
- `ignore` — ripgrep's parallel directory walker.
- `ratatui` + `crossterm` — TUI.
- `clap`, `serde`, `toml`, `serde_json` — plumbing.

Fetch and pull **shell out to `git`** rather than using gix. The user's SSH
keys, credential helpers and `insteadOf` rules already work with the `git`
binary; reimplementing that is risk with no upside.

### Discovery

`ignore::WalkBuilder` walking each root in parallel. It honours `.gitignore`,
`.ignore`, and the global gitignore. On top of that:

- Directory names in the prune list are never descended into. This is what
  keeps terragrunt's cached module clones out of the results.
- **A directory containing `.git` is recorded as a repository and not
  descended into.** Nested repos, vendored clones and submodules therefore
  never appear as their own rows.
- `.git` as a *file* (worktrees, submodules) counts as a repository.

Unreadable directories are collected into a problems list, not printed.

### RepoStatus

Computed for every repo found, on a `num_cpus`-sized worker pool.

```
path, display_name          # display_name is the path relative to its root
head                        # Branch(name) | Detached(short_sha) | Unborn
upstream                    # Option<String>
ahead, behind               # u32, 0 when no upstream
staged, unstaged, untracked, conflicted   # u32 counts
stash_count                 # u32
last_commit_time            # Option<SystemTime>
fetch_age                   # Option<Duration>, from FETCH_HEAD mtime
error                       # Option<String>; a failed repo is a row, not a crash
```

### RepoDetail (lazy)

Computed for the selected repo only, on a worker thread, then cached:
local branches with their ahead/behind vs upstream, the stash list (read
only), remotes with URLs, the last ~10 commits, and the names of changed
files (capped).

### Drift scoring

Score descending; ties broken by `last_commit_time` descending. Clean,
in-sync repos sort last.

| Condition | Weight |
|---|---|
| conflicted > 0 | 1000 |
| diverged (ahead > 0 and behind > 0) | 500 |
| ahead > 0 | 100 |
| unstaged or staged > 0 | 50 |
| detached HEAD | 40 |
| behind > 0 | 25 |
| no upstream | 20 |
| untracked > 0 | 10 |
| stash_count > 0 | 5 |

Weights are additive and live in one table in `drift`, adjustable without
touching anything else.

## Colour

Colour is load-bearing here, not decoration: at 500 rows it is how a repo's
state is read. One meaning, one colour, one glyph — used identically in the
TUI, the detail pane and `--plain`.

| State | Colour | Glyph |
|---|---|---|
| conflicted | red, bold | `✖N` |
| diverged | red | `⇅N/M` |
| ahead (unpushed) | bright yellow | `↑N` |
| behind (unpulled) | blue | `↓N` |
| staged | green | `✚N` |
| unstaged | yellow | `●N` |
| untracked | cyan | `?N` |
| stashed | magenta | `⚑N` |
| detached / no upstream | bright magenta | `⌀` |
| clean and in sync | dim green | `✔` |
| error | red on dim | `!` |

Row-level cues: the repo's basename is bold and coloured by its *worst*
state, so severity is readable without parsing the counter column; the
leading path segments (`sre/`, `platform/`) are dimmed so the eye lands on
the name. The selected row is reversed in an accent colour. Section headers
and keybinding hints in the footer use dim/accent, never a status colour —
status colours mean status and nothing else.

Rules:

- **Never colour-only.** Every state carries a distinct glyph, so the display
  survives colourblindness, `NO_COLOR`, and piping.
- Honour `NO_COLOR` and `--color=never`; `--color=auto` (default) disables
  colour when stdout is not a tty.
- Truecolor where available, with a 256- and 16-colour fallback. Choose
  colours that hold up on both light and dark terminal backgrounds.
- The palette lives in `theme` as named semantic constants (`CONFLICT`,
  `AHEAD`, …). No literal colour anywhere else in the codebase.

## Data flow

The walker streams repo paths into a bounded channel; the status pool
consumes them and pushes `RepoStatus` results to the UI over another channel.
The TUI paints on the first frame with a "scanning…" indicator and fills in
as results arrive, re-sorting on a ~100ms debounce so the list does not
jitter under the cursor. Detail for the selected repo is computed off-thread
and cached, so navigation never blocks.

`--plain` and `--json` drain the same channels to completion, then print.

## Keybindings

| Key | Action |
|---|---|
| `↑`/`↓`, `k`/`j` | move selection |
| `g`/`G` | first / last |
| `Enter` | print selected path to stdout and quit (`cd $(gitdrift)`) |
| `f` | fetch selected repo |
| `F` | fetch all repos |
| `p` | `pull --ff-only` on selected repo |
| `s` | open `$SHELL` in the repo; TUI restored on exit |
| `e` | open the configured editor in the repo |
| `r` | rescan |
| `d` | toggle drifted-only |
| `/` | filter by path substring; `Esc` clears |
| `!` | problems log |
| `?` | help overlay |
| `q`, `Ctrl-C` | quit |

Network operations are capped at `fetch_concurrency` (default 8) in flight;
`F` across 510 repos must not open 510 SSH sessions. Progress is reported in
aggregate in the footer, with a rescan (`r`) picking up the new counts.

`p` refuses, with a stated reason, when the worktree is dirty, when there is
no upstream, or when the merge would not be a fast-forward.

## Error handling

- A repository that fails to open or stat becomes an error row; the scan
  continues. The reason appears in the detail pane.
- Walk errors (permissions, broken symlinks) go to the `!` problems log.
- Action failures raise a transient footer toast and retain full stderr in
  the detail pane until the next action on that repo.
- The engine never calls `exit`; only `main` decides the process exit code.
  Non-zero exit if every root was unreadable, zero otherwise.

## Testing

Fixture repositories are built into tempdirs by a helper that shells out to
real `git`, covering: clean, ahead, behind, diverged, staged-only,
unstaged-only, untracked-only, conflicted, stashed, detached HEAD, unborn
HEAD (no commits), no-upstream, and a repo nested inside another.

- `status` tests assert every `RepoStatus` field against those fixtures.
- `drift` tests assert ordering for a hand-built set of statuses, including
  tie-breaking by commit time.
- `discover` tests assert the prune list is honoured, `.gitignore` is
  honoured, nested repos are not descended into, and `.git`-as-a-file is
  detected.
- `ui` state is tested as pure `(state, event) → state` transitions; no pty.
- `render` tests assert `--json` shape and that `--color=never` emits no
  escape sequences.

## Open risks

- gix's API surface is still evolving; a breaking release may need a pinned
  version and a deliberate bump. Mitigated by keeping all gix calls inside
  `status` and `detail`.
- Ahead/behind counts are only as fresh as the last fetch. `fetch_age` is
  displayed so a stale count is visibly stale rather than silently wrong.
