# arnes is a practical bidama host

Operator, 2026-09-28: *"everything in arnes should be configurable and
programmable with bluelang and be a bluelang extension or module of some kind
in order to make arnes fully hackable and definable"*, then: *"this should all
be bidamas, entire thing should be a bunch of bidama, arnes should in a lot of
ways just be a practical bidama hoster"*, and *"re-use all the bidamas we have
and integrate and refactor all toward extreme code efficiency and re-use"*.

So arnes is two things: a Rust **kernel** that hosts bidamas, and a set of
**bidamas** that are arnes's features. Every feature a user sees is a function
in a bidama, resolved by name, replaceable by a bidama of the same name, and
configurable off. This plan follows `theory/BLUE.md` §I.1 (reuse map, shape,
idioms, dependency order) and extends `docs/programmability.md`, which built the
seam (blue's `hosted` API, user commands, tools, hooks, vigies).

## 1. What the kernel is

The kernel is what blue cannot express, or what enforces an invariant a bidama
must not be able to break. Its size is ratcheted (operator, 2026-10-08: as much
blue as possible, as little Rust as possible): `kernel-baseline.txt` holds each
crate's Rust line count and the host-function count, each row under the
world-fact that keeps it Rust, and the `kernel-ratchet` generator fails when a
measurement passes its row. A row in the table below and a baseline edit land
together, or the Rust does not land. It is the host: it loads, resolves, budgets and
capability-scopes bidamas, and reloads them.

| kernel piece | where | why it is kernel |
|---|---|---|
| bidama hosting: resolution by name, embedded distribution, budgets, effect application, capability scope | `arnes-blue` (`bidamas`, `within_budget`) | the host itself |
| terminal I/O: raw mode, protocols, key decode, paste, editor hand-off | `arnes/src/main.rs`, egaku-term | owns the tty |
| render and layout engine: region grid, drawers, diffed frames, pacing, colour depth, motion clock | `arnes-tui`, egaku-term | enforces the screen law; a bidama returns text, never paints |
| markdown and code layout | mojiban via `egaku_term::markdown` | engine; a theme bidama feeds it |
| editor core: text area, vim engine, history, kill ring | `arnes-tui`, escriba | per-keystroke hot path |
| navigation grammar | egaku `Navigator` | one grammar on every surface |
| provider transport: wire, streaming, retries, fallback execution, each route's auth header (`x-api-key` or bearer), every failure judged by `handan` into one typed verdict (`unauthorized`, `throttled`, `server-error`, `unreachable`, `timed-out`), kept per route as its health beside the route that served the turn and its latency | `arnes-anthropic` | network and secrets |
| MCP client and pool | `arnes-mcp-client` | processes and sockets |
| tool loop, permission gate, guardrail | `arnes-loop` | a bidama may veto, never grant |
| ledger, sessions, layout, retention; each call recorded against the route that served it, at that route's declared price | `arnes-loop/ledger.rs`, `arnes-store` | durable formats, atomic writes |
| config loading, hot reload, routes as data (the `messages` provider, route facts) | `arnes-config`, shikumi | the declaration layer |
| vigy runtime embedding | `arnes-vigy`, vigy-runtime | the scheduler is vigy's |

## 2. The bidamas

arnes's own distribution lives in this repo at `bidamas/`, one directory per
bidama (`Bluefile`, `<name>.b`, tests), with a `NAMES.md` ledger, the way
makoto keeps its private distribution. It is arnes's because every bidama in it
calls the `arnes_*` host API, which exists only inside arnes; a generic shape
found while writing one moves into blue's public `bidamas/` instead (§3).

| bidama | kanji | what | status |
|---|---|---|---|
| `meirei` | 命令 | every built-in slash command: `/clear`, `/exit`, `/help`, `/mcp`, `/mode`, `/model`, `/notices`, `/profile`, `/resume`, `/vigy`, `/vim` | **landed** |
| `joutai` | 状態 | status-line segments: model, effort, tools, permission mode, profile, over one `joutai::segment` | **landed** |
| panels | | `/vigy`, `/mcp` views | H7, H11 |
| activity | | the activity line's text | H8 |
| notices | | notice policy: keep, fold, drop, lifetime | H8 |
| `kenban` | 鍵盤 | the default key bindings, one `bind <action> <chord>` export each | **landed** |
| routes | | route ranking, start-on-free policy, `/model` | H6 |
| tools | | declaring the built-in tools, blue tools | H10 |
| `sanshou` | 参照 | `@path` mentions attached to a prompt, a before-submit hook | **landed** |
| `kenkou` | 健康 | the MCP health vigy (`vigy mcp-health tick 30000`) and the `mcp` segment, hosted by default: a tick reads the health scheduler's facts and probes nothing (docs/profiles.md) | **landed** |
| theme | | any bidama with `color <slot> <#RRGGBB>` rows, hosted with `:bidamas`, sets palette slots over the fleet theme `:tema` names; the palette then feeds `egaku_term::markdown` | mechanism landed (H12); no theme ships by default |

Names for the rest are minted with the `naming` sweep when each lands, with a
`NAMES.md` row in the same commit.

**How the kernel hosts them.**

- **Resolution by name.** A `use("name")` resolves through `BidamaPath`, first
  match wins: the user's `$XDG_CONFIG_HOME/arnes/bidamas/<name>`, then
  `$ARNES_BIDAMAS` (a path list, what a nix install sets), then the copy
  compiled into the binary, then `$BLUE_PATH` (blue's standard and private
  distributions). So a user replaces a whole bidama by dropping a directory of
  the same name, and any arnes program can `use` blue's standard bidamas.
- **Exports.** Each hosted bidama says what it serves in `exports()`: one
  line per entry, `<kind> <name> <function> <description>` (`command clear
  clear start a new conversation`). Each bidama is a namespace, so the
  function is its bare name and the kernel calls it as `<bidama>::<function>`
  (`meirei::clear`), the way a program writes it. The kernel reads them at
  start and on reload into a registry; a bad line or a
  bidama that will not load is refused alone, named in a notice, and the rest
  are hosted. The `/` picker lists the hosted commands with their own
  descriptions. `:bidamas ("mine")` hosts more bidamas beside `meirei`,
  `joutai` and `kenban`. A user `defarnes-command` of the same name replaces one command.
- **Configured off, never deleted.** `:modules-off ("help" "tools")` turns an
  entry off by name. The Rust twin of each ported feature stays in the tree,
  served only with `:kernel-fallback #t` (default off), so revival is one field.
- **Budgets.** Every call from the prompt runs on a worker with a wall-clock
  budget (`:module-budget-ms`, default 250). Over it, the effects are dropped
  and a budget-stage error names the bidama. The program is interrupted too:
  every interpreter a budgeted worker builds halts on one flag
  (tatara-lisp-eval's `Interpreter::set_interrupt`, built into the evaluator
  for this), set when the budget runs out, so its next evaluation step raises
  `EvalError::Halted`, no `catch` intercepts it, and the worker ends.
- **Isolation.** Each call builds a fresh interpreter and resolves its
  bidamas again, so an edited bidama takes effect on the next call with no
  reload.

## 3. Reuse map

Read before writing each bidama: `blue/bidamas/CATALOG.md` and a
`^def name(` search across blue's and makoto's distributions (the `bidama`
skill). Kept current each rung.

| arnes bidama | composes | what for |
|---|---|---|
| `yushutsu` | `moji` (`lines`, `words`) | the one export-row shape every hosted bidama writes and its tests read back |
| `meirei` | `yushutsu`, `ronri` (`every`) | exports as rows; tests over them |
| `joutai` | `yushutsu`, `ronri` | exports as rows; tests over them |
| `kenban` | `yushutsu`, `ronri`, `moji` (`includes`) | the bindings as rows; tests over them |
| `kenkou` | `yushutsu`, `deeta` (`get_str`, `get_int`) | total JSON reads of each probe |
| `sanshou` | `yushutsu`, `moji` (`words`, `starts_with`, `ends_with`, `take_chars`, `drop_chars`, `includes`), `retsu` (`contains`, `push`) | tokenising and de-duplicating mentions |

blue's whole standard distribution is on every hosted bidama's path at run
time with nothing installed: blue-lang-pkg's `embedded` feature compiles it
into arnes (`embedded::Standard`), after `$BLUE_PATH` so a checkout still
wins.

Shapes to watch, extracted on the second use: a segment separator (now
`joutai::segment`); JSON reads of host state (use `deeta`'s total readers, never
raw `json_get`); lists of sessions and notices (use `retsu`, `junjo`); session
and ledger analysis (use `kueri`, `nisshi`, `bunseki`).

## 4. The host API, by capability

Reads are values captured before the call; writes are **effects** applied after
a successful call, so a bidama that raises changes nothing.

| capability | calls | kind | rung |
|---|---|---|---|
| conversation, prompt, store | `arnes_conversation`, `arnes_hint`, `arnes_toast`, `arnes_insert`, `arnes_store_*` | read / effect | done |
| lifecycle | `arnes_clear()`, `arnes_exit()` | effect, runs the kernel's own arm | done |
| panels | `arnes_panel(title, text)` | effect, a navigable panel | done |
| keymap read | `arnes_bindings()` | read | done |
| files | `arnes_read_file(path, max)`: a text file of at most `max` bytes, else `nil` | read; the only filesystem access a hosted bidama has | done |
| state and settings | `arnes_state(key)` (model, effort, permission-mode, tools, vim, busy), `arnes_set(key, value)` (vim, permission-mode) | read / effect the kernel validates, then runs the arm it replaces | done |
| notices | `arnes_notices()` | read | done |
| sessions | `arnes_sessions()`, `arnes_resume(query)` | read / effect, runs the kernel's resume arm | done |
| routes | `arnes_state("route")`, `arnes_route_switch(name)`, `arnes_pick("model")` | read / effect, runs the kernel's model arm (catalogue, client rebuild, live switch) | done |
| profiles | `arnes_pick("profile")`, `arnes_profile(name)` | effect, runs the kernel's profile arm (the file re-applied with the overlay, live) | done |
| policies | hosted `hook` exports on `mcp-lazy`, `mcp-probe`, `choose-profile`, `skills`, `choose-route`, asked through `arnes_blue::policy` (`policy_with` for `choose-route`, which carries the reach) | a pure answer: first text wins, nil abstains, an effect is refused | done |
| kernel views | `arnes_open(view, args)` for `mcp` and `vigy` | effect, opens the kernel's own view | done |
| network | `arnes_http(endpoint, json)`: POSTs `json` to a declared `endpoints` entry and returns `{status, verdict, ms, body}`; the key is read in Rust under `access.key-files` and never returned; at most 4 calls per invocation; a keyed plain-http URL off this machine is refused | gated call, choose-route hooks only: every other caller is refused | done (R6) |
| MCP | `arnes_mcp_servers()`, `arnes_mcp_tools(s)`, `arnes_mcp_call(s, t, json)` | read / gated call | H7 |
| time and motion | `arnes_now_ms()`, a segment returning a blip name | read / effect | H8 |

**Capability scope.** Each hosted entry gets only the calls its kind allows,
and the kind is declared in its export: a `command` may use every command
capability; a `segment` returns text, and one that asks for any effect is
refused with nothing applied. A hook or a tool inside a turn cannot clear,
exit or open a panel (a named warning). New kinds (hook, vigy, keymap, route)
arrive with their rungs, each with its own scope.

**Invariants.** A bidama never paints (regions cannot overlap). Every tool
call goes through the gate. Every call has a budget. Store bounds are per
bidama (256 keys, 64 KiB). Every failure is typed, names the bidama and the
stage, and lands in the notice lane.

## 5. Rungs, in dependency order

Each rung is small, tested (empty case, identity, independent value, control,
each red once) and committed alone.

| rung | what | status |
|---|---|---|
| H1 | the four ported features as blue behind the kernel, Rust twins behind `:kernel-fallback`, `:modules-off` | done (`6a51fec`) |
| H1b | worker plus wall-clock budget | done (`4cafc8c`) |
| H2 | the features move into real bidamas (`meirei`, `joutai`) in `bidamas/`, resolved by name through `BidamaPath`, user bidamas override by name | done (`f404cd3`) |
| H2b | a root `Bluefile` lowered by `blue.lib.project`: per-bidama tests, collisions, locks, catalogue as flake checks, `nix run .#regen` | done (`f268bc6`) |
| H2c | blue's standard distribution compiled into arnes (blue-lang-pkg feature `embedded`, built into blue), so a hosted bidama `use`s any public bidama with nothing installed; the export-row shape the three bidamas repeated extracted into `yushutsu`, and they are refactored onto it and `ronri`/`moji` | done (`bc1b044`) |
| H3 | exports: `<name>_exports()` lines, the registry, kind-scoped capabilities, the picker from exports, `:bidamas` | done (`8e6c12b`) |
| H4 | state, settings and notices API; `/vim`, `/mode`, `/notices` in `meirei`; model, effort and permission segments in `joutai`, each redrawn only when its argument changes and budgeted like a command | done (`83762ee`) |
| H5 | sessions API; `/resume` in `meirei`: no argument opens a navigable sessions panel (the kernel twin toasted each line), an argument runs the kernel's resume arm | done (`375c4b1`) |
| H6 | `/model`, `/mcp` and `/vigy` in `meirei` over `arnes_pick`, `arnes_route_switch` and `arnes_open`; every kernel-arm effect is one typed `Effect::Arm`. Every built-in command is now a bidama command. The start-on-free policy stays kernel until a `choose-route` hook kind (H6b) | done (`7478172`) |
| H6b | closed without code: the configuration rung made the free local route the default, so there is no start-on-free choice left to make, and the route's label reaches blue through `arnes_state("route")` and the `idle` segment | closed; reopened as R7 |
| R7 | the `choose-route` policy event: with `routing.enabled` and a hosted hook, each new prompt (submit and queued) is asked off the UI thread with the session's facts (`arnes/src/routing.rs`, v1) under `routing.budget-ms`; a declared label moves the turn through the `/model` path, nil, an error or an undeclared label keeps the route; every decision goes to `routes.jsonl` (tail dropped, its size kept). The queued path now runs hosted before-submit hooks too. One-shot `-m` runs on the configured route | done |
| H7 | the `mcp <up>/<all>` status segment is `kenkou`'s, fed the probe facts JSON, proven equal to the kernel twin, which stays for when `kenkou` is not hosted. The `/mcp` browser and `/vigy` panel stay kernel widgets (navigation, live refresh); their text moving to a bidama is H7b | done (`6ac574c`) |
| H8 | the idle activity text is `joutai`'s `idle` segment, redrawn when the route changes. The busy text (spinner, tool, clocks) is per-frame and stays engine; notice policy stays the declared knobs (lifetimes per severity, lane rows), already configuration | done (`8f112c6`) |
| H9 | the default keymap is `kenban`'s `bind` exports, proven equal to the kernel's; `keys.prompt` edits apply on top; `kenban` off keeps the kernel keymap. Mention expansion (a before-submit hook kind) is H9b | done (`373ab8a`) |
| H9b | a `hook` export kind; `@path` mention expansion is `sanshou`'s before-submit hook (over `moji`, `retsu`), proven to attach exactly what the kernel twin did; files come through `arnes_read_file(path, max)`, the one filesystem capability a hosted bidama has (blue's `sys` layer is not linked). `sanshou` off runs the kernel twin | done (`62bbb62`) |
| H10 | each built-in tool (Read, Write, Edit, Glob, Grep, Bash) can be named in `:modules-off`: not served, not sent to the model, and its name free for a blue tool that replaces it. The tools stay Rust (filesystem, subprocess, guardrail), so there is nothing for a bidama to declare beyond the name | done (`4629e77`) |
| H11 | a `vigy` export kind; hosted vigies run as `bidama:<package>` scripts on the shared runtime beside declared ones, a declared vigy winning its name; MCP health moved into `kenkou` over `deeta`, and `examples/vigy/mcp-health.b` reuses it | done (`b9c2260`) |
| H12 | a `color` export kind: `(slot, #RRGGBB)` rows over the fleet palette, each bad row refused alone, applied at start and on reload | done (`a622691`) |
| H13 | arnes on namespaced blue: every bidama defines bare names (`meirei::clear`), `exports()` names them, and the kernel calls `<bidama>::<function>` through one `run_bidama` (blue's `Hosted::call` reads a qualified name); each host-calling definition carries `# waive B0001: arnes host call` so `blue test` still checks it outside arnes; the six bidamas move to 0.2.0 | done |

A word blue lacks while porting goes into blue or a public bidama, not into
arnes.
