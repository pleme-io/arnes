# Persistence, caching and the filesystem

Operator, 2026-09-28: *"see the optimization between vigy getting data and then
caching and storing data and we need some sort of concept of persistence in
arnes and caching and also management of the filesystem and things we keep
there."*

This is the plan, fixed before code (`theory/BLUE.md` §I.1): the reuse map, the
shape, the rung each piece is on, and the dependency order.

## Reuse map (read from source 2026-09-28)

| exists | where | decision |
|---|---|---|
| `ledger::default_path()` | `arnes-loop/src/ledger.rs` | `$XDG_STATE_HOME/arnes/usage.jsonl`. Moves into the layout; the ledger keeps its format |
| `notice::log_path()` / `mcp_log_dir()` | `arnes/src/notice.rs` | derived from the ledger path by `with_file_name`, so the state dir was in one place by accident. Now named by the layout |
| `catalog::cache_path()` + `load_or_refresh` | `arnes-anthropic/src/catalog.rs` | the one TTL cache: fresh if younger than `max_age`, else fetch, and a failed fetch falls back to the stale copy. Its path moves into the layout |
| `Conversation` / `Message` derive serde | `arnes-core` | a session is a serialized `Conversation`; no type changes |
| shikumi `ConfigDiscovery` | shikumi | owns config discovery. There is no state/cache/data helper anywhere in the fleet, and no shared paths crate, so arnes names its own |
| acervo `ObjectStore` (BLAKE3) | `galho/crates/acervo` | the content-addressed store to adopt when arnes has a blob worth addressing. Not today: no arnes consumer needs one, so nothing is built |
| `VigyStore` runs + kv | `vigy-store` | the shared result cache between hosts: every tick's `VigyRun` is persisted in one SQLite file every host opens |
| `nisshi`, `kueri` bidamas | `blue/bidamas` | blue-side JSONL log and DuckDB queries, for analysing the ledger and sessions from blue |

## One filesystem layout

`arnes_store::Layout` resolves four roots once, from the XDG variables with the
`$HOME` fallbacks, and every path arnes writes is a typed method on it. Nothing
else in arnes joins a path under these roots.

| root | default | holds | bound |
|---|---|---|---|
| config | `$XDG_CONFIG_HOME/arnes` | `arnes.{yaml,lisp,nix}` (shikumi discovers it), the user's `.b` files | the user's; never written by arnes |
| state | `$XDG_STATE_HOME/arnes` | `usage.jsonl` (ledger), `arnes.log`, `mcp/<server>.log`, `sessions/<id>.json`, `history.jsonl` (prompts) | logs rotate at 8 MiB to one `.1`; sessions keep the newest 200 and none older than 90 days; the prompt history keeps its newest 10000 lines (`history.keep`); the ledger is kept whole (it is the analysis source) |
| cache | `$XDG_CACHE_HOME/arnes` | `models-<provider>.json` | TTL one day; safe to delete at any time |
| data | `$XDG_DATA_HOME/arnes` | `ext/<extension>.json` (blue extension state) | 256 keys and 64 KiB per extension, refused at write |

The vigy store is not arnes's: it stays at `$VIGY_DB`, else
`~/.local/share/vigy/vigy.db`, the rule mado and the `vigy` CLI already follow,
so all three open one file.

**Retention is configured, not hard-coded.** Four flat keys, each independent
(`:sessions "off"` stops recording; `:sessions-keep`, `:sessions-max-age-days`,
where 0 means unbounded; `:log-max-mb`). They are flat, not one nested block,
so a bad value is refused alone and its default used instead of taking the
document down with it (the scoped-refusal rule). The sweep
(`arnes_store::sweep`) runs once at startup on its own thread and logs what it
removed at debug level; a log rotated then starts fresh on the next start.

## Persistence model

- **Sessions.** Each TUI session is one file, `sessions/<id>.json`: a header
  (id, cwd, created, updated, a title from the first prompt) and the
  `Conversation`. It is rewritten atomically (temp file, then rename) after
  every finished turn, so a crash loses at most the turn in flight. The id is
  the timestamp and pid, the same one the ledger records, so a session joins
  its usage rows.
- **Resume.** `arnes --resume` reopens the latest session for the current
  directory; `arnes --resume <id-prefix>` a named one. In the TUI, `/resume`
  lists recent sessions and `/resume <n|id>` switches to one. `/clear` starts a
  new session file and leaves the old one on disk.
- **Prompt history.** One file for every directory, `history.jsonl`, one
  line per submitted prompt (`at`, `cwd`, `text`), appended by each arnes
  process with one write, the way a session header carries its `cwd`.
  `history.scope` decides which lines a session walks (this directory's
  first, by default), and `Ctrl-R` ranks them with `wadachi_spec::usage`,
  the same ranking the shell's `Ctrl-R` (skim-tab) uses. A line that does
  not parse is skipped alone. Compaction to `history.keep` is a rewrite
  (temp file, then rename) at startup; a prompt another process appends
  during that rewrite can be lost.
- **Extension state.** Blue code gets `arnes_store_get(key)`,
  `arnes_store_set(key, value)` and `arnes_store_del(key)`, namespaced per
  extension (the command, tool or hook name). Writes follow the effects rule:
  they are collected while the program runs and committed only when it
  succeeds, so a raising program leaves the store as it found it. Keys and
  values are strings, bounded as above.
- **Vigy results.** Persisted by vigy-store, never copied into arnes files.

## Cache model

| cached | key | freshness | on failure |
|---|---|---|---|
| model catalogue | provider name | one day (`--models` forces a refresh) | the stale copy, named as stale |
| MCP probe facts | server name | refreshed by the TUI every time a server connects, is listed or is probed | the last known facts, with their age |
| vigy results | `VigyId` (BLAKE3 of name and program) | one tick | the last run, with its time |

`arnes_store::cache::load_or_refresh` is the catalogue's cache lifted out
(fresh, else fetch and write back, else the stale copy) behind a `Stamped`
trait. The catalogue is its only disk consumer today; it moved into the store
because the path already lived there, not to share it, and it leaves arnes
only when a second crate needs it. MCP facts are an in-memory cache and vigy
results live in vigy.db, so neither is a second consumer. Content addressing enters through the keys that are
already content-derived (`VigyId`); a blob store waits for a blob.

## The vigy data path

```
probe (arnes, async)  ->  facts cache (in memory, per server)
                               |
vigy tick (host=arnes) --reads-+--> blue fn --> summary + per-server observed
                               |
                         VigyRun persisted in vigy.db  --->  mado, CLI, any host
                               |                               (reads, never re-probes)
                         broadcast --> arnes status segment and /vigy panel
```

- **Fetch once.** Only arnes probes its MCP servers, because only arnes holds
  the connections. The facts go into an in-memory cache the vigy's host
  primitive reads synchronously (vigy intrinsics run under `spawn_blocking`).
- **Tick once, across hosts.** The vigy carries `host=arnes`. vigy-runtime's
  host selector (`open_with_extensions_for`) means mado, which opens the same
  store, lists and shows the vigy but never schedules it, so nothing is ticked
  twice and nothing runs where its primitives are missing.
- **Store once, read anywhere.** The run and its actions land in `vigy.db`.
  That file is the shared result cache: a host that wants MCP health reads
  `recent_runs`, it does not probe.
- **Surface per tick.** arnes subscribes to the runtime's bus and redraws the
  status segment and `/vigy` panel on each run.

## Rungs

| piece | rung | why |
|---|---|---|
| host selector | Rust (vigy-runtime) | the scheduler is Rust; the selector is one predicate on its resume and spawn paths |
| layout, retention, sessions, cache, extension store | Rust (`arnes-store`) | host effects: filesystem, atomic rename, mtimes. This is the seam blue stands on |
| `arnes_store_*` | Rust primitive, blue surface | the primitive is the seam; everything built on it is blue |
| the MCP health vigy | blue | the policy (what counts as down, what the summary says) is blue; the stub vigy program in the store is generated, never hand-written |
| probing | Rust | it needs the live MCP connections arnes holds |

## Dependency order

1. vigy: tatara-lisp 0.3.60 (so vigy links beside blue), then the host selector.
2. arnes: drop the private moldura input (nix build).
3. `arnes-store`: layout, and the existing paths move onto it; retention.
4. Sessions and resume (`--resume`, `/resume`).
5. The cache lifted from the catalogue.
6. `arnes_store_*` for blue.
7. The vigy rung: embed vigy-runtime for `host=arnes`, the `arnes` extension,
   the blue-authored MCP health vigy, the surfaces.
