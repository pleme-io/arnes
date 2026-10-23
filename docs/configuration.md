# arnes configuration

Every knob arnes has, in one place. The type is `arnes_config::ArnesConfig`
(`arnes-config/src/lib.rs`, sections in `sections.rs`); `arnes config schema`
prints it as JSON schema.

## Where it comes from

shikumi finds `$ARNES_CONFIG`, else `$XDG_CONFIG_HOME/arnes/arnes.{lisp,yaml,nix,toml}`.
All four formats load into the same struct. A value is resolved in tiers, the
shikumi standard for apps:

```
tier (ARNES_TIER: bare | discovered | default)  ->  ARNES_* env  ->  the file
```

`default` is the prescribed tier and what `Default::default()` returns.
`bare` has no opinion (no theme, no max-tokens, the keyless local provider).

**A bad value is refused alone.** The file is applied one key at a time; a key
whose value does not fit is refused by its dotted path with serde's reason,
its default is kept, and every other key loads. Sections are descended into,
so a bad leaf costs only that leaf. Lists keep every entry that fits (a
malformed keybinding or fallback is skipped by index). Unknown keys are named
with the nearest known key (`tem: unknown key, ignored (did you mean tema?)`).
Each refusal is a notice in the TUI. Only a file that does not parse at all is
refused whole, and then the running config is kept. Bounds are checked where a
value is read, again one value at a time (`picker-rows 99 is outside 3..=40;
using 12`).

**A nested key wins over its flat twin**, and the flat one keeps working:
`:ui (:motion "off")` over `:motion "off"`, `:persistence (:sessions "off")`
over `:sessions "off"`. Nothing was removed to add the sections.

## `arnes config`

| command | what |
|---|---|
| `arnes config show [--format lisp\|yaml\|nix\|json]` | the effective config (tier, env, file) |
| `arnes config show --sources` | one line per value: `ui.motion = "off"  # file` (default, env or file) |
| `arnes config validate [FILE]` | load as the TUI would; every refusal listed; exit 1 when any, 2 when it does not parse |
| `arnes config schema` | JSON schema of every key |
| `arnes config tier bare\|discovered\|default` | a shikumi tier on its own |

`--config PATH` before the subcommand points it at a file.

## Live

The TUI watches the file (a home-manager rebuild re-renders it) and applies a
change in place: no restart, no new conversation, the prompt, scrollback,
notices, extension state and session file untouched. What each part does:

| change | applied how |
|---|---|
| provider, model, endpoint, key, route note, fallbacks | the client is rebuilt; the conversation moves to the new route and the next turn goes there. Thinking blocks the new route cannot accept (another provider's signed reasoning, where the new route echoes history) are dropped and a notice says how many |
| sampling, reasoning, transport | the client is rebuilt; a thinking level cycled with Alt-T is kept unless the file changed it |
| `mcp_servers`, `anvil_json` | the pool is reconciled in the background: only new or changed servers start, only removed or changed ones stop, unchanged ones keep their connection and tools. The activity line names what started and stopped; a server that cannot start is a warning. A turn in flight keeps the pool it started with |
| `mcp.exposure`, `builtin_tools`, blue `tools`, `before-tool` hooks, `guardrail` | the gate and tool list are rebuilt for the next turn; "always" answers and the permission mode carry over |
| permission mode and rules | swapped in the gate |
| `ui.*`, `keys.*`, `activity.*` | swapped in the state; an open surface, picker or `/mcp` browser reads the new keys from the next press |
| blue commands, submit and after-turn hooks, `modules_off`, `module_budget_ms`, `bidamas` | looked up again for the next call (hosted bidamas re-read their exports) |
| `persistence.sessions` | recording stops, or starts a session file for the running conversation |
| retention (`sessions-keep`, `sessions-max-age-days`, `log-max-mb`) | a sweep runs now |
| `vigies` | redeclared on the running vigy runtime: new or edited ones register, removed ones are disabled (kept with history), the rest keep ticking; the first one declared starts the runtime |
| `/model` | the same route switch as a reload: conversation kept, next turn to the new route |
| `/profile`, `--profile` | the file re-applied with the profile's overlay through this same reload path: MCP servers diffed, gate, tools, route, bidamas, skills and context swapped for the next turn, conversation kept |
| `skills.*`, `context.*` | rediscovered; the system prompt and the `Skill` tool change from the next turn |
| `history.*` | `search`, `ranking`, `pin_recent` from the next `Ctrl-R`; a new `scope` or `keep` re-reads the history file (everything recorded is on disk); `record` off stops writing, on starts, and neither drops what the session already walks |

**Not live, and why** (each named in a notice when changed):

| key | reason |
|---|---|
| `persistence.ledger` / `:ledger` | the running session keeps writing the usage file it opened, so its totals stay one record; the new path is used from the next start |

## Provider and routes (flat keys)

| key | type, bound | default | notes |
|---|---|---|---|
| `provedor` | `local` \| `deepseek` \| `anthropic` \| `messages` | `local` | the free local route is the default; a paid provider is used once configured with its key; `messages` is any Anthropic-format gateway (OpenRouter): it names its own `endpoint` and `modelo`, takes a key, and sends it as bearer |
| `modelo` | string | the provider's (`local`, `deepseek-flash`, `claude-opus-5-5`) | checked against the provider's live catalogue at startup |
| `endpoint` | URL | the provider's (`http://127.0.0.1:8080` for local) | |
| `api_key_file` | path | none | wins over the env var |
| `key_env` | env var name | `LOCAL_LLM_API_KEY` / `DEEPSEEK_API_KEY` / `ANTHROPIC_API_KEY` | |
| `api_version` | string | `2023-06-01` | |
| `system_note` | string | none | added to the system prompt while this route answers; travels with the route |
| `prompt_sistema` | string | none | the system prompt |
| `max_tokens` | u32 | 8192 | |
| `fallbacks` | list of routes | none | tried in order on a failover-class error; each: `provedor modelo endpoint api_key_file key_env timeout_secs system_note tools label facts`; an unresolvable route (a `messages` route without `endpoint` or `modelo`, an unknown `auth`) is refused alone |
| `temperatura` / `top_p` / `top_k` / `stop_sequences` | 0..=2 / 0..=1 / u32 / strings | provider's | out of range refused by name |
| `thinking` / `effort` / `thinking_budget` / `echo_thinking` | on\|off / low\|high\|max / u32 / bool | provider's | |
| `timeout_secs` | u64 | none | whole request |
| `connect_timeout_secs` | 1..=120 | 10 | a route that does not accept a connection this fast is unreachable |
| `max_retries` / `retry_base_ms` | u32 / u64 | 3 / 500 | |
| `extra_headers` | `(:name :value)` list | none | |

### `route` and route `facts`: what a route is, as data

The primary's own `route` section, and `facts` on each fallback, carry what a
route judge and the ledger read. Switching a route (`/model`, failover at
start) moves the section with it, so a no-tools route stays no-tools as the
primary.

| key | type, bound | default | notes |
|---|---|---|---|
| `route.label` | string | the provider's name | the primary's name in notices, the status line and route picks |
| `route.tools` | bool | on | off: the primary is sent no tool list |
| `facts.auth` | `x-api-key` \| `bearer` | bearer for `messages`, `x-api-key` for the rest | an unknown value refuses the route |
| `facts.describe` | string | none | what the model is good at, in a sentence: the text a route judge reads |
| `facts.tier` | 0..=9 | none (ranked by price) | strength rank among the routes |
| `facts.price_in` / `price_out` / `price_cache` | USD per Mtok | none | the ledger's price for this route |
| `facts.context_window` | tokens | none | |

### `free_route`: when nothing configured can serve

When the provider's key cannot be read and no fallback resolves, arnes starts
on the free route instead of exiting, and says so. A local endpoint that is
not reachable is a notice before the first turn and a one-line failure after
it (start `llama-server`, or `/model`), never a crash.

| key | type | default |
|---|---|---|
| `free_route.enabled` | bool | on (off: an unusable provider is an error at startup) |
| `free_route.endpoint` | URL | `http://127.0.0.1:8080` |
| `free_route.on_device` | bool | on on macOS |
| `free_route.on_device_endpoint` | URL | `http://127.0.0.1:8434` |
| `free_route.on_device_model` | string | `apple-on-device` |

`catalog.max_age_hours` (0..=720, default 24): how long a cached model
catalogue is trusted. `--models` always refreshes.

## `ui`

| key | type, bound | default | flat twin |
|---|---|---|---|
| `ui.theme` | fleet theme: `pleme_dark` (`dark`), `vellum`, `polar_veil`, `bare`, `nord` | `dark` | `tema` |
| `ui.motion` | on \| off | on | `motion` |
| `ui.notifications` | auto \| off | auto | `notifications` |
| `ui.editor_mode` | normal \| vim | normal | `editor_mode` |
| `ui.regions.notice_rows` | 0..=10 | 3 | `notice_rows` |
| `ui.regions.picker_rows` | 3..=40 | 12 | `picker_rows` |
| `ui.notices.info_secs` | 1..=600 | 4 | `notice_info_secs` |
| `ui.notices.warn_secs` | 1..=600 | 8 | `notice_warn_secs` |
| `ui.notices.error_secs` | 1..=600 | 12 | `notice_error_secs` |
| `ui.timing.frame_rate` | 1..=240 | 60 | |
| `ui.timing.idle_poll_ms` | 10..=1000 | 50 | |
| `ui.timing.reveal_row_ms` | 0..=200 | 12 (clamped by the fleet's fast/slow tokens) | |

The 150 ms acknowledgements, panel opening and toast slide use the fleet
motion tokens (`ishou-tokens` fast/base/slow), so every egaku-term program
moves alike; `ui.motion "off"` turns them all off.

## `activity`

| key | bound | default |
|---|---|---|
| `activity.shimmer_ms` | 200..=10000 | 1800 |
| `activity.hold_ms` | 100..=10000 | 1200 (a note stays twice this) |
| `activity.notify_after_secs` | 0..=3600 | 10 (shorter turns finish without a desktop notification) |

## `keys`

Three keymaps, each edited the same way: `unbind` removes chords from the
defaults, then `bind` points chords at action ids. Each entry is refused alone
(unknown action, unparsable chord, a chord not bound).

| section | reads | action ids |
|---|---|---|
| `keys.prompt` | the prompt (`arnes_tui::Keymap`, Claude Code defaults) | `app:interrupt app:exit chat:cancel app:redraw app:toggleTranscript chat:thinkingToggle app:cycleMode app:scrollback chat:submit chat:newline chat:externalEditor chat:stash history:previous history:next history:search edit:left edit:right edit:wordLeft edit:wordRight edit:lineStart edit:lineEnd edit:deleteBack edit:deleteForward edit:killToEnd edit:killToStart edit:killWordBack edit:killWordForward edit:yank edit:yankPop edit:undo` |
| `keys.nav.menu` | help, panels, `/mcp`, the approval question, the scrollback (`egaku::NavKeymap::menu`) | `nav:down nav:up nav:top nav:bottom nav:halfPageDown nav:halfPageUp nav:pageDown nav:pageUp nav:viewTop nav:viewMiddle nav:viewBottom nav:search nav:searchNext nav:searchPrev nav:open nav:back nav:dismiss nav:close nav:yank` |
| `keys.nav.filter` | pickers, where letters filter (`NavKeymap::filter`) | the same `nav:*` ids |

Chords are awase's spelling: `ctrl+x ctrl+e`, `shift+tab`, `G`, `g g`.
Footers and `/help` are rendered from the keymaps, so a rebinding shows there.

```lisp
:keys (:prompt (:bind (("ctrl+q" "app:exit")) :unbind ("ctrl+d"))
       :nav (:menu (:bind (("x" "nav:close")) :unbind ("q"))
             :filter (:unbind ("tab"))))
```

## `mcp`

| key | bound | default | flat twin |
|---|---|---|---|
| `mcp_servers` | list of `(:nome :command :args :env :cwd :required)` | none | |
| `anvil_json` | path | none | merged after `mcp_servers` |
| `mcp.exposure` | auto \| all \| deferred | auto | `mcp_tools` |
| `mcp.inline_max` | 1..=2000 | 40 (auto sends schemas inline up to this many tools) | |
| `mcp.connect_timeout_secs` | 1..=300 | 15 | |
| `mcp.call_timeout_secs` | 1..=3600 | 120 | |
| `mcp.startup` | background \| block | background: the TUI draws at once, each server attaches as it comes up (`MCP x up (n/m)`); block waits for all first. One-shot `-m` always waits | |
| `mcp.lazy` | globs | none: servers not started until their first call; the model sees them named in `mcp_call` | |
| `mcp.only` / `mcp.off` | globs | none (all) / none | |
| `mcp.scopes` | anvil scope names | none: each reads `mcp.scope_dir/<scope>.json`, after `anvil_json` | |
| `mcp.scope_dir` | path | `$XDG_CONFIG_HOME/arnes/mcp` | |
| `mcp.project` | bool | off: read `./.mcp.json` (a cloned repository does not start processes unasked) | |
| `mcp.health.enabled` | on \| off | on | |
| `mcp.health.probe_min_ms` / `probe_max_ms` | 1000..=3600000 / 1000..=86400000 | 15000 / 300000: a healthy server's interval grows from the first to the second | |
| `mcp.health.growth_pct` | 100..=1000 | 200 (doubling per ok probe) | |
| `mcp.health.fail_ms` / `reconnect_max_ms` | 100..=600000 / 1000..=3600000 | 2000 / 120000: a failure is retried at the first, doubling to the second | |
| `mcp.health.jitter_pct` | 0..=100 | 10 | |
| `mcp.health.quarantine_flaps` / `quarantine_window_secs` / `quarantine_secs` | 0..=100 / 1..=86400 / 1..=86400 | 4 / 300 / 600: that many up/down flips in the window quarantine a server, one notice | |
| `mcp.health.timeout_ms` | 100..=60000 | 2000 | |
| `mcp.health.lease` | on \| off | on: routine probes of a healthy server are shared with other arnes processes through `vigy.db` | |

Discovery reads `mcp_servers`, `anvil_json`, the scope files and (with
`mcp.project`) `./.mcp.json`, first name wins; the same command, arguments and
environment under a second name is dropped and named. A failed tool call probes
that server at once. The blue policies are hosted `hook` exports
(docs/profiles.md): `mcp-lazy` (receives the spec as JSON, returns `lazy`,
`eager` or nil) and `mcp-probe` (receives `{server, up, streak, interval_ms}`,
returns the next interval in ms or nil); a policy that asks for an effect is
refused. `kenkou` is hosted by default and ticks every 30 s, reading the
scheduler's facts.

## Tools, permissions, extensions

| key | default | notes |
|---|---|---|
| `builtin_tools` | on | Read/Write/Edit/Glob/Grep/Bash |
| `guardrail` | on | guardrail's production rules screen Bash and Write/Edit |
| `permission_mode` | default | default \| plan \| bypass; Shift-Tab cycles |
| `allow_tools` / `deny_tools` | none | globs; deny beats allow |
| `commands` / `tools` / `hooks` | none | blue extensions (docs/programmability.md), each refused alone |
| `vigies` | none | blue reconcilers ticked by the shared vigy runtime (docs/persistence.md) |
| `modules_off` | none | turned off by name: hosted bidama entries (commands, segments), a hosted bidama's key bindings (`kenban`), or a built-in tool (`Bash`), whose name a blue tool may then take (docs/kernel.md) |
| `kernel_fallback` | off | serve a module that is off from its Rust twin |
| `module_budget_ms` | 250 | wall-clock budget for one blue command or segment call |
| `bidamas` | none | bidamas to host beside `meirei` and `joutai`, by name; re-read on reload |
| `turn.max_rounds` | 16 (1..=256) | most model calls in one turn |
| `mentions.max_files` | 20000 (1..=1000000) | files the `@` picker lists |
| `mentions.max_kib` | 256 (1..=16384) | largest file an `@path` attaches |
| `extras` | none | free-form strings for agent-specific knobs |

## Profiles

A profile is a named overlay onto this file (docs/profiles.md): any top-level
key or section, applied over its `:inherits` parents, each value refused alone
when it does not fit. A profile cannot set `profiles` or `profile`.

| key | type | default |
|---|---|---|
| `profile` | a declared name | none: the profile used when `--profile` (or `$ARNES_PROFILE`), this directory's last choice (`$XDG_STATE_HOME/arnes/profiles.json`) and the `choose-profile` hook name none |
| `profiles` | list of `(:name :description :inherits :config)` | none |

`/profile` opens a picker of the declared profiles (`-` is none); `/profile
NAME` switches, and remembers it for the directory. The status line shows
`profile NAME` (joutai's `profile` segment). The hosted `choose-profile` hook
receives `{cwd, repo, hour, profiles}` and returns a name or nil.

## `skills`

| key | bound | default |
|---|---|---|
| `skills.enabled` | bool | on |
| `skills.dirs` | paths | `~/.claude/skills`, `./.claude/skills` (each holds `<skill>/SKILL.md`; the first of a name wins) |
| `skills.only` / `skills.off` | globs | none (all) / none |
| `skills.max_listed` | 0..=2000 | 200 |

Each skill's name and description (its SKILL.md frontmatter, parsed by
skill-lint) is listed in the system prompt; the read-only `Skill` tool returns
a body. A file without a name or description is named and skipped. The hosted
`skills` hook receives the list as JSON and returns the names to keep, one per
line.

## `context`

| key | bound | default |
|---|---|---|
| `context.files` | paths | none: each appended to the system prompt under its path |
| `context.discover` | bool | on: `CLAUDE.md` and `AGENTS.md` in the working directory |
| `context.prompt` | string | none: appended after the files |
| `context.max_kib` | 1..=4096 | 64 |

The system prompt is `prompt_sistema`, then the context files, then
`context.prompt`, then the skills listing.

## `access`

| key | default | bounds |
|---|---|---|
| `access.read_roots` | none (anywhere) | `arnes_read_file`, `@path` attachments (both the `sanshou` hook and the kernel twin) and context files read only under these directories |
| `access.key_files` | none (any) | the API key files a route may read; a route naming another is refused alone |

Permission mode, `allow_tools`/`deny_tools`, `mcp.only`/`mcp.off` and
`modules_off` are the rest of a profile's access; they are ordinary keys.

## `persistence`

| key | default | flat twin |
|---|---|---|
| `persistence.sessions` | on | `sessions` |
| `persistence.sessions_keep` | 200 (0 = unbounded) | `sessions_keep` |
| `persistence.sessions_max_age_days` | 90 (0 = never) | `sessions_max_age_days` |
| `persistence.log_max_mb` | 8 | `log_max_mb` |
| `persistence.ledger` | `$XDG_STATE_HOME/arnes/usage.jsonl`; `off` | `ledger` |

## `history`

What `Up`/`Down` walk and `Ctrl-R` searches. The keys are `keys.prompt`
(`history:previous`, `history:next`, `history:search`; defaults `up`/`ctrl+p`,
`down`/`ctrl+n`, `ctrl+r`).

| key | bound | default |
|---|---|---|
| `history.record` | on \| off | on: every submitted prompt is one line of `$XDG_STATE_HOME/arnes/history.jsonl` (`at`, `cwd`, `text`), shared by every arnes process and recalled in later sessions. Off: this session's prompts only, nothing read or written |
| `history.scope` | project-then-global \| project \| global | project-then-global: `Up` walks this directory's prompts newest first, then every other directory's; the picker ranks this directory's first |
| `history.keep` | 0..=1000000 (0 = unbounded) | 10000 newest lines; the file is rewritten to that once it holds half as many again |
| `history.search` | picker \| incremental | picker: `Ctrl-R` opens the ranked fuzzy picker (the typed line is its query; `Ctrl-R` again moves to the next row; `Enter` puts the prompt in the box to review). incremental: reverse-i-search in the prompt box |
| `history.ranking` | frecency \| recency | frecency: `wadachi_spec::usage`, the ranking skim-tab gives the shell's `Ctrl-R` (time-decayed use count) |
| `history.pin_recent` | bool | on: the newest prompt stays first whatever its score |

`Up` on the first row recalls the previous prompt with the cursor on its first
row, so a multi-line prompt is walked past rather than climbed; `Down` on the
last row recalls the next with the cursor on its last row. Past the newest, the
prompt being typed comes back.

## `routing`: a route for each prompt

Off by default. On, and with a bidama exporting `hook choose-route`, each new
prompt (never a tool round) is offered to the hook with the session's facts
before the turn starts; its answer, a declared route's label or nothing, moves
the turn exactly as `/model` would. The knobs after `log` are passed to the
hook as `knobs`; the kernel does not read them.

| key | type, bound | default | notes |
|---|---|---|---|
| `routing.enabled` | bool | off | the master switch |
| `routing.budget_ms` | 100..=5000 | 1000 | the whole choice, judge call included; over it, the route is kept |
| `routing.tail_kib` | 1..=64 | 16 | how much of the conversation the facts carry |
| `routing.log` | path \| `off` | `$XDG_STATE_HOME/arnes/routes.jsonl` | one line per decision, facts and outcome, the tail dropped |
| `routing.judge` | an `endpoints` name | none | none: the hook's floor alone decides |
| `routing.min_confidence` | 0..=100 | 70 | |
| `routing.stick_turns` | 0..=50 | 3 | |
| `routing.cooldown_secs` | 0..=86400 | 300 | |
| `routing.allow_paid` | bool | off | |

## `endpoints`: what a route judge may reach

`(defarnes-endpoint :name "judge" :url "https://api.example.com/v1/judge" :key-file "~/.config/example/api-key")`.
The only network a hosted bidama has: `arnes_http(name, json)` from a
`choose-route` hook. An endpoint that cannot be resolved is refused by name
and the others are kept.

| key | type, bound | default | notes |
|---|---|---|---|
| `name` | string | — | what the hook passes to `arnes_http` |
| `url` | URL | — | a key is never sent over plain http to a host off this machine |
| `auth` | `x-api-key` \| `bearer` | bearer | |
| `key_file` / `key_env` | path / env var | none (no key) | `key_file` must be inside `access.key-files`; the key never reaches blue |
| `timeout_ms` | 100..=10000 | 700 | out of range refuses the endpoint |
| `max_kib` | 1..=4096 | 256 | a larger reply comes back as `too-large` with no body |

## Fixed by design

| value | where | why it is not a knob |
|---|---|---|
| tool names `^[a-zA-Z0-9_-]{1,64}$` | arnes-mcp-client | the providers' rule |
| `mcp_search` returns 1..=25, default 8 | arnes-mcp-client | the meta-tool's contract with the model |
| extension store: 256 keys, 64 KiB per extension | arnes-store | a bound on what blue code may persist, the same for every extension |
| motion durations (fast/base/slow) | ishou-tokens | fleet-wide, so every TUI moves alike |

## Home-manager

`programs.arnes` (this repo's `module/default.nix`) has a typed option for
every key above: the flat ones by name (`provider`, `model`, `fallbacks`,
`connectTimeoutSecs`, …) and every section as a typed submodule
(`programs.arnes.ui.regions.pickerRows`, `programs.arnes.keys.nav.menu.bind =
{ "x" = "nav:close"; }`). Options and rendering come from one spec per
section, and a crate test fails when the schema has a key the module lacks.
`extraConfig` stays as the escape hatch. `programs.arnes.profiles.<name>`
is typed the same way (`config` takes every section and the flat keys a profile
usually changes), and `mcpScopes` renders one `~/.config/arnes/mcp/<scope>.json`
per scope through anvil's `serversForScope`, which `mcp.scopes` reads.

## Example

```lisp
(defarnes
  :provedor "local"
  :endpoint "http://gpubox:8080"
  :system-note "You are gpt-oss-20b on gpubox."
  :fallbacks ((:provedor "local" :label "on-device" :endpoint "http://127.0.0.1:8434"
               :modelo "apple-on-device" :tools #f)
              (:provedor "deepseek" :api-key-file "~/.config/deepseek/api-key"))
  :ui (:theme "vellum" :editor-mode "vim"
       :regions (:notice-rows 2 :picker-rows 16)
       :notices (:error-secs 20))
  :activity (:notify-after-secs 30)
  :keys (:prompt (:bind (("ctrl+q" "app:exit")))
         :nav (:menu (:bind (("x" "nav:close")))))
  :mcp (:exposure "auto" :inline-max 60 :connect-timeout-secs 10)
  :persistence (:sessions-keep 50)
  :turn (:max-rounds 24))
```

The same in YAML:

```yaml
provedor: local
endpoint: http://gpubox:8080
ui:
  theme: vellum
  regions: { notice_rows: 2, picker_rows: 16 }
keys:
  nav:
    menu:
      bind: [["x", "nav:close"]]
mcp: { inline_max: 60 }
```
