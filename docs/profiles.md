# MCP detection and profiles

Operator, 2026-09-28: *"we have to be intelligent and async in detecting mcp
servers and we should actually be able to switch between profiles in arnes that
load and unload different mcp servers, skills, and contexts"*, then
*"configurations access"* and *"all should be configurable and programmable
shikumi bluelang style"*.

This is the plan, fixed before code (`theory/BLUE.md` §I.1): reuse map, shape,
idioms, dependency order. Every knob below is a `docs/configuration.md` row, a
typed field in `arnes-config` and a typed leaf in `module/default.nix`; every
policy is a blue hook with the Rust side only the mechanism.

## Reuse map (read from source, 2026-09-28)

| exists | where | decision |
|---|---|---|
| `McpPool::reconcile(previous, next)` | `arnes-mcp-client` | the one diff: kept, started, stopped. A profile switch and a reload both go through it; nothing else starts or stops a server |
| `McpPool::attach` (concurrent connect of specs) | `arnes-mcp-client` | split so one server's connection arrives on its own ([`McpPool::connect_server`]); startup attaches each arrival instead of waiting for all |
| `McpPool::probe` | `arnes-mcp-client` | kept; a per-server `probe_one` is added for the scheduler |
| `Live::reload(Loaded)` | `arnes/src/main.rs` | the live-apply path. A profile switch builds a `Loaded` (the file's config with the profile's overlay) and hands it to `reload`, so route, gate, tools, keys, bidamas and MCP swap exactly as on a file edit, and the conversation is kept |
| `salvage::salvage(base, overlay)` | `arnes-config` | the scoped refusal. A profile's `:config` overlay is applied with it, so one bad key in a profile is refused alone and the rest applies |
| hosted `hook <event> <fn>` exports, `Registry::hooks(event)` | `arnes-blue` | every policy is a hook event a bidama exports: `mcp-probe`, `mcp-lazy`, `choose-profile`, `skills`, `choose-route` |
| `kenkou` | `bidamas/kenkou` | the MCP health vigy and segment; hosted by default once probing is adaptive |
| vigy store `load_kv` / `save_kv` | `vigy-store` | the fleet-wide probe lease, keyed by the kenkou vigy's id, in the one `vigy.db` every host opens |
| `skill_lint::SkillFrontmatter::parse` | `skill-lint` 0.1.5 (crates.io) | the fleet's SKILL.md frontmatter parser; arnes does not write one |
| anvil `serversForScope` | blackmatter anvil | the nix module renders one file per declared scope, `~/.config/arnes/mcp/<scope>.json`; a profile names scopes |
| `arnes_store::Layout` | `arnes-store` | the last profile per directory is `state/profiles.json` |

## 1. MCP detection

### Sources, deduplicated

`to_mcp_registry` reads, in order, first name wins:

| source | what | knob |
|---|---|---|
| `config` | `:mcp-servers` | always |
| `anvil` | `:anvil-json` (the file anvil renders for arnes) | always when set |
| `scopes` | `mcp.scope-dir/<scope>.json` for each `mcp.scopes` | `mcp.scopes` |
| `project` | `./.mcp.json` in the working directory (Claude Code's project file) | `mcp.sources` |

A second entry with the same name is dropped; a second entry with a different
name but the same command, arguments and environment is dropped too and named
(the same server declared twice would start twice). Then `mcp.only` (globs;
empty = all) and `mcp.off` (globs) select. `mcp.sources` turns a source off.

### Startup never blocks

`mcp.startup "background"` (default): the TUI draws at once with an empty
pool; each server connects on its own task and is attached as it arrives
(`arrival`), the gate and tool list rebuilt over it, the activity line saying
`MCP github up (3/7)`. `"block"` keeps the old behaviour (wait for all before
the first frame); one-shot `-m` always blocks, since its one turn needs the
tools.

### Lazy servers

`mcp.lazy` (globs) names servers that are not started at startup. They are
listed to the model as dormant in the `mcp_search` description; the first
`mcp_call` or `mcp__<server>__*` call to one starts it (once, shared by
concurrent callers) and the call proceeds. A profile switch that selects a
lazy server leaves it dormant. The blue hook `mcp-lazy` receives the server
spec as JSON and returns `"lazy"`, `"eager"` or nil (the globs decide).

### Adaptive health

One `Health` state per server (`arnes_mcp_client::health`), pure and tested at
pinned instants:

| event | next probe |
|---|---|
| probe ok | interval x `factor`, capped at `probe-max-ms` (idle, healthy: rare) |
| probe failed / call error | `fail-ms` now, then reconnect with backoff doubling to `reconnect-max-ms` |
| state flips `quarantine-flaps` times within `quarantine-window-secs` | quarantined for `quarantine-secs`, one warning notice, no probes; then retried |

Every interval gets up to `jitter-pct` percent of deterministic jitter (a hash
of the server and the attempt), so several arnes processes do not probe in
step. A tool call that fails on a server marks it suspect and probes it now
(event-driven): the call is the best probe there is.

**One probe per server, fleet-wide.** Routine probes of a healthy server take a
lease in `vigy.db` (`mcp-lease/<fingerprint>`, the kenkou vigy's kv): a lease
another live arnes took within the interval means the upstream was just seen
healthy, so this process skips its routine probe and extends its interval.
Failure-driven probes never skip, since they check this process's own
connection. `mcp.health.lease "off"` turns sharing off.

The blue hook `mcp-probe` receives `{server, up, streak, interval_ms}` and may
return an interval in ms, overriding the computed one for that server.

## 2. Profiles

A profile is a named config overlay with inheritance. Everything a profile can
change is already a config key, so a profile is data, not a new mechanism:

```lisp
(defarnes
  :profile "base"
  :profiles ((:name "base" :description "pleme-io defaults"
              :config (:mcp (:scopes ("pleme")) :skills (:only ("arnes" "bidama"))))
             (:name "ops" :inherits ("base") :description "operations, read-mostly"
              :config (:mcp (:scopes ("ops") :only ("codesearch" "atlassian" "github"))
                       :skills (:only ("ops-*" "kit"))
                       :context (:files ("~/code/github/example-org/CLAUDE.md")
                                 :prompt "Terragrunt, never pangea.")
                       :permission-mode "plan"
                       :deny-tools ("Bash")
                       :access (:key-files ("~/.config/deepseek/api-key")
                                :read-roots ("~/code/github/example-org"))
                       :provedor "deepseek"))))
```

- **Compose.** `:inherits` lists parents applied first, left to right; the
  profile's own `:config` last. A cycle or an unknown parent is refused by name
  and the rest of the chain applies.
- **Apply.** The effective config is `salvage(file, overlay)`, key by key (a
  bad key in a profile is refused alone). It goes through `Live::reload`, so
  MCP servers are diffed (unchanged kept, the rest started or stopped), the gate
  and tools rebuilt, the route switched, bidamas re-hosted, skills and context
  swapped for the next turn, and the conversation kept.
- **Choose.** `--profile NAME`, else the last profile used in this directory
  (`state/profiles.json`), else the hosted `choose-profile` hook (it receives
  `{cwd, repo, hour}` and returns a name or nil), else `:profile`, else none.
- **Switch.** `/profile` (`meirei`) opens a picker on the shared egaku
  navigation (`arnes_pick("profile")`); `/profile NAME` switches
  (`arnes_route`-style kernel arm `profile`). The status line's `profile`
  segment is `joutai`'s.
- **Access.** `access.read-roots` (globs of directories) bounds
  `arnes_read_file` and `@path` attachments; `access.key-files` bounds which key
  files a route may read (a route naming another is refused alone);
  `permission-mode`, `allow-tools`, `deny-tools`, `mcp.only/off` and
  `modules-off` are ordinary keys a profile overlays.

## 3. Skills

A skill is a directory holding `SKILL.md`: YAML frontmatter (`name`,
`description`, parsed by `skill-lint`) and a markdown body. arnes reads the
Claude Code locations, so the fleet's skills (deployed by blackmatter to
`~/.claude/skills`) work unchanged:

| key | default |
|---|---|
| `skills.enabled` | on |
| `skills.dirs` | `~/.claude/skills`, `./.claude/skills` |
| `skills.only` / `skills.off` | globs over names; empty only = all |
| `skills.max-listed` | 200 |

The names and descriptions are listed to the model in the system prompt; the
`Skill` tool (`{name}`) returns the body on demand. A skill without a name or
description is skipped with a notice naming the file. The hosted hook `skills`
receives the listed skills as JSON and returns the names to keep, one per line
(nil keeps all).

## 4. Context

| key | default |
|---|---|
| `context.files` | none; each file (`~` expands) is appended to the system prompt under its path |
| `context.discover` | on: `CLAUDE.md` and `AGENTS.md` in the working directory |
| `context.prompt` | none; a fragment appended after the files |
| `context.max-kib` | 64 per file |

## Rungs, in dependency order

| rung | what | status |
|---|---|---|
| P1 | `arnes-mcp-client`: `health` state machine; registry `merge`/`dedup`/`select`; `ServerSpec.lazy`, dormant servers and connect-on-call; `connect_server` + `with_arrival`; `probe_one` | done (`297026e`) |
| P2 | config: `mcp.startup/lazy/only/off/scopes/scope-dir/project/health.*`; the binary attaches arrivals, the scheduler (`arnes/src/mcp_health.rs`) replaces the fixed 5 s probe; kenkou hosted by default at 30 s; `arnes_blue::policy` and the `mcp-probe` and `mcp-lazy` hooks | done (`6bb0ac0`) |
| P3 | the probe lease in `vigy.db` (`arnes_vigy::Lease`) | done (`9e6252e`) |
| P4 | profiles: `ProfileDecl` (`defarnes-profile`), deep overlay and inheritance, `--profile`, per-directory memory (`arnes_store::profiles`), `choose-profile`, `/profile` on the shared picker, `profile` segment, `access.*` | done |
| P5 | skills and context: `skills.*` (`arnes_tools::skills` over skill-lint), the `Skill` tool in the gate, the `skills` hook, `context.*` in the system prompt | done |
| P6 | nix options for every new key and for `profiles` (typed, the overlay reusing the section specs); `docs/configuration.md` rows; skill and doc pointers | done |

## Example profile

```lisp
(defarnes
  :profile "pleme"
  :profiles ((:name "pleme" :description "pleme-io: blue, bidamas, codesearch"
              :config (:mcp (:scopes ("pleme") :lazy ("splunk-*"))
                       :skills (:only ("arnes" "bidama" "pleme-*" "blue-*"))))
             (:name "ops" :inherits ("pleme") :description "operations, read-mostly"
              :config (:mcp (:scopes ("ops") :only ("codesearch" "atlassian" "github"))
                       :skills (:only ("ops-*" "kit" "stacked-prs"))
                       :context (:files ("~/code/github/example-org/CLAUDE.md")
                                 :prompt "Terragrunt, never pangea. Plan before apply.")
                       :permission-mode "plan"
                       :deny-tools ("Bash")
                       :access (:read-roots ("~/code/github/example-org")
                                :key-files ("~/.config/deepseek/api-key"))
                       :provedor "deepseek"))))
```

The same in Nix:

```nix
programs.arnes = {
  profile = "pleme";
  profiles.pleme = { description = "pleme-io"; config.mcp.scopes = [ "pleme" ]; };
  profiles.ops = {
    inherits = [ "pleme" ];
    config = { permissionMode = "plan"; denyTools = [ "Bash" ]; mcp.only = [ "codesearch" ]; access.readRoots = [ "~/code/github/example-org" ]; };
  };
};
```

A policy bidama choosing the profile from the directory:

```blue
use("moji", [:includes])

def exports()
  "hook choose-profile choose"
end

def choose(facts)
  cwd = json_get(json_parse(facts), "cwd")
  if includes(cwd, "example-org")
    "ops"
  else
    nil
  end
end
```
