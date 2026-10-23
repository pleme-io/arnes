# arnes is programmable in blue

Operator, 2026-09-28: arnes is shikumi-programmable, meaning tatara-lisp
programmable and blue programmable, with blue as the primary way to implement
anything in arnes. This is the plan, fixed before code (`theory/BLUE.md` §I.1:
reuse map, shape, idioms, dependency order).

## The split: declared in config, implemented in blue

| layer | what lives there | language |
|---|---|---|
| declaration | that an extension exists, its name, where its code is, when it fires | `(defarnes …)` tatara-lisp, or YAML/Nix, through shikumi |
| implementation | what the extension does | blue (`.b`) |
| host seam | the typed primitives blue calls, running a program, turning its effects into TUI state | Rust (`arnes-blue`), the one rung blue cannot reach |

A declaration is data, so it is a `#[derive(DeriveTataraDomain)]` struct in
`arnes-config` and needs no code. Logic is blue. Rust holds only the seam.

## Reuse map (read from source, 2026-09-28)

| exists | where | used for |
|---|---|---|
| `#[blue_lang_host::host(prefix = "arnes_")]` | blue `crates/blue-lang-host` | the host's primitives are the methods of one impl block on `H`: each becomes a blue host function with typed arguments, a typed result and a `Result` error naming the function; `Host::SURFACE` lists them, and a test pins it to the `arnes_*` names |
| `blue_lang_runtime::interpreter(&mut host)` | blue `crates/blue-lang-runtime/src/lib.rs` | the only sanctioned way to build an interpreter carrying a host type `H` |
| parse, check, erase, run in the one order | `pipeline::run_in_surface` | the default path. It evaluates with a hostless interpreter only, so a host could not reuse it |
| `use("pkg")` resolution | `blue_lang_pkg::LoadPath::from_env()` (`BLUE_PATH`) | user scripts can pull bidamas from the standard or a private distribution |
| JSON in blue | `json_parse` / `json_stringify` (runtime layer 4) | a tool's input arrives as a JSON string and a script parses it |
| `DeriveTataraDomain`, shikumi `ProviderChain` | `arnes-config` | one struct, three authoring formats, hot reload |
| `ToolDispatcher`, `GatedDispatcher` | `arnes-loop` | a blue tool is gated exactly like a built-in or MCP tool |
| toasts, hints, prompt editing | `arnes-tui` `TuiState::toast`, `InputState::hint`, `TextArea::insert_str` | where a script's effects land |

**Other Rust hosts embedding blue today: none.** Searched the pleme-io tree for
`blue_lang_runtime` / `blue-lang-` outside blue itself: only blue's own crates
(cli, test, lsp, wasm, pkg) and `sui-fmt` (syntax only, no runtime). So there is
no second embedding to reuse, and the risk is inventing one privately in arnes.
The seam is therefore built **into blue**: `blue_lang_runtime::hosted`, which
runs the same pipeline as `run_in_surface` into an interpreter the host built
and then calls a named definition with arguments. `run_in_surface` and
`load_hosted` share one prepare stage, so the order stays in one place.

## The shape

### Rung 1: slash commands and model-callable tools

```lisp
(defarnes
  :commands ((:name "wc" :description "count words in the conversation"
              :blue "~/.config/arnes/wc.b" :fn "run"))
  :tools    ((:name "shout" :description "upper-case a text"
              :blue "~/.config/arnes/shout.b" :fn "shout"
              :schema "{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}"
              :read-only #t)))
```

```blue
# ~/.config/arnes/wc.b
def run(args)
  words = length(split(arnes_conversation(), " "))
  arnes_toast("info", "#{words} words so far")
  "#{words} words"
end
```

```blue
# ~/.config/arnes/shout.b
def shout(input)
  upcase(json_get(json_parse(input), "text"))
end
```

The same entries stand alone as `(defarnes-command …)`, `(defarnes-tool …)` and
`(defarnes-hook …)`, prefixed because `defcommand`, `deftool` and `defhook` are
already claimed in the fleet's keyword ledger (escriba, kura, frost). YAML and
Nix say the same keys.

- A command's function receives the argument string typed after `/name`.
- A tool's function receives its input as a JSON string (`json_parse` it).
- Both return a string: the command's result becomes a hint (or a toast when
  long), the tool's result is the tool result the model sees.
- A command appears in the `/` picker beside the built-ins. A built-in name
  wins, and a declaration that shadows one is refused with a toast.

**Host API** (flat namespace, one prefix, `arnes_`):

| primitive | effect |
|---|---|
| `arnes_conversation()` | the conversation so far as plain text, `role: text` per message |
| `arnes_hint(text)` | the one-line status hint |
| `arnes_toast(level, text)` | a toast; level is `"info"`, `"warn"` or `"error"`, anything else is a typed error |
| `arnes_insert(text)` | insert text into the prompt at the caret |
| `arnes_store_get(key)` | this `.b` file's saved string for `key`, or `nil`; sees the call's own earlier writes |
| `arnes_store_set(key, value)` | save a string under `key`; kept in `$XDG_DATA_HOME/arnes/ext/<file stem>.json` |
| `arnes_store_del(key)` | forget `key` |

Store writes follow the effects rule: saved only when the call succeeds, at most 256 keys and 64 KiB per file, and a call over the bound fails at the `store` stage with nothing saved (`docs/persistence.md`).

Effects are collected while the program runs and applied afterwards, so a
program that raises halfway applies nothing but the error.

**Errors are typed toasts, never silent.** Every failure names the extension
and its stage: the file cannot be read, a parse or type error (with
`file:line:col`), an import error, a runtime raise, the function is not
defined, the function returned something that is not a string. For a tool the
same text is the error the model receives.

**Isolation:** each invocation builds a fresh interpreter and loads the file
again (about 10 ms, measured by blue as the stdlib build). No state leaks
between calls, and an edited `.b` takes effect on the next call with no reload.

### Rung 2: hooks

```lisp
:hooks ((:on "before-submit" :blue "hooks.b" :fn "tidy")
        (:on "after-turn"    :blue "hooks.b" :fn "log_turn")
        (:on "before-tool"   :blue "hooks.b" :fn "guard"))
```

| event | receives | returns |
|---|---|---|
| `before-submit` | the prompt | the prompt to send (a rewrite), or `nil` to send it unchanged |
| `after-turn` | nothing (reads `arnes_conversation()`) | ignored; effects apply |
| `before-tool` | tool name, input JSON | `"allow"`, or anything else as the reason to deny |

An unknown `:on` is refused by name for that entry alone; the other hooks still
load. A hook that fails is a toast, and a failing `before-tool` hook denies
(fail closed), naming the hook. A failing `before-submit` hook sends nothing and
puts the text back in the prompt.

`before-tool` is a veto, not a grant: it runs after the allow/deny rules and
guardrail and before the human is asked, so `"allow"` means "no objection" and a
side-effecting tool is still asked in default mode. Hooks chain in declaration
order (each `before-submit` sees the previous one's prompt; the first
`before-tool` deny wins). `before-submit` and `after-turn` follow the config on
reload; `before-tool` hooks live in the gate and change on restart.

### Rung 3: ticking (vigy)

Operator, 2026-09-28: integrate vigy and vigy-style applications, authored in
blue, unified with the other vigy hosts, absorbing data and showing it per
tick. First consumer: MCP server health.

**Reuse map (read from source 2026-09-28):**

| exists | where | fact that decides the design |
|---|---|---|
| `RuntimeHandle::open_with_extensions(path, exts)` | `vigy-runtime/src/lib.rs:108` | the scheduler: one tokio task per enabled vigy, backoff 1 s to 30 s, `subscribe()` broadcasts every `VigyRun`, `recent_runs(id, n)` reads the last results. arnes reuses it and writes no scheduler |
| `HostExtension` / `closure_extension` | `vigy-eval/src/lib.rs:248,951` | how a host adds primitives. They are synchronous and run under `spawn_blocking`, so an async probe reads a cache the host fills |
| `Vigy { name, program: String, tick_interval, labels }` | `vigy-types/src/vigy.rs:49` | a program is tatara-lisp source text, re-read every tick; the id is derived from name and program, so re-registering is idempotent |
| the shared store | `mado/src/vigy_host.rs:119` | the platform mado and the `vigy` CLI share is one SQLite file: `$VIGY_DB`, else `~/.local/share/vigy/vigy.db`. There is no daemon or socket. tear names a reconciler actor (`tear-types/src/shutai.rs:79`) and embeds nothing |
| `vigy::mcp::dispatch(rt, name, args)` | `vigy-mcp/src/lib.rs:108` | the same `vigy_*` tool surface mado exposes, callable in process |

**Unify, do not fork.** arnes opens the same store by the same path rule, so a
vigy registered from mado, the CLI or arnes is listed and inspectable in all of
them. **The gap this exposes, which belongs in vigy, not arnes:** every process
that opens the store resumes a tick task for every enabled vigy
(`vigy-runtime/src/lib.rs:127`), so two hosts tick each vigy twice, and a vigy
that calls one host's primitives fails in the other. The fix is a host
selector in vigy itself: a vigy carries a `host` label and a runtime opened
with `open_with_extensions_for(path, exts, host)` resumes only the vigies whose
label it serves (unlabelled ones keep today's behaviour). arnes's vigies carry
`host=arnes`.

**Authoring in blue.** Blue lowers to the tatara-lisp AST vigy-eval already
runs, so a vigy body is blue compiled to the program text the store holds
(parse, check, erase, print). One short declaration per vigy:

```lisp
:vigies ((:name "mcp-health" :blue "~/.config/arnes/health.b" :fn "tick" :every-ms 5000))
```

and where a tatara-lisp vigy spells out observe, desire, act and condition by
hand, the words it repeats (`vigy-observed` then `vigy-condition` then
`vigy-mark-converged`) become one blue form built into blue or vigy, so the
per-vigy code is only what differs.

**The live surface.** An `arnes` host extension gives vigies the MCP facts
arnes already holds, from a cache the TUI fills: per server, up or down, the
latency of a cheap probe (`tools/list`), tool count, last error. The results
feed the `/mcp` browser's `ServerStatus`, a compact status-line segment
(`mcp 5/6`), and a `/vigy` panel listing each vigy, its interval and its last
run. All three update on every tick through `subscribe()`.

**The tatara-lisp version is not a blocker (measured 2026-09-28).** mado pins
`tatara-lisp-eval` below 0.3.58 for an E0433 of its own, and blue needs 0.3.60,
which suggested arnes could not link both. But vigy requires only `0.3.3`, and
with its lock moved to 0.3.60 the crates arnes would link (vigy-types, -store,
-eval, -runtime, -mcp) build and pass their tests, 46 of 46. vigy-rpc fails
only for want of `protoc`, and arnes takes vigy as mado does, without it
(`default-features = false, features = ["mcp"]`).

**Status (2026-09-28).** Shipped: vigy on tatara-lisp 0.3.60, the host
selector (`open_with_extensions_for`, `serves`; an unlabelled vigy is
scheduled only by a runtime opened without a host), and the embedding in the
`arnes-vigy` crate. `every-ms` defaults to 5000. The stored program is
generated from the declaration, one call to the `arnes-blue` intrinsic, so the
author writes only the blue function, which receives the probe facts as JSON
(`json_parse`) and returns the tick's text; `examples/vigy/mcp-health.b` is the
first. arnes probes its servers (`McpPool::probe`, a timed `tools/list`) on the
shortest declared interval, and only when a vigy is declared. `/vigy` lists
each vigy and its last result, `/vigy tick <name>` ticks one now, and a failing
tick raises a warning toast. Still to come: the status-line segment and a
`/vigy` panel on the shared menu navigation. The data path is in
`docs/persistence.md`.

### Later rungs

| extension point | declaration | blue receives / returns |
|---|---|---|
| status-line segments | `:segments ((:blue … :fn …))` | nothing / the segment text, re-evaluated at most once a second |
| keybindings | `:keys ((:chord "ctrl-g" :command "wc"))` | binds a chord to a declared command, so it is config only |

## Idioms

- One prefix per namespace: host primitives are `arnes_*`.
- Typed errors that name the extension, the stage and the cause.
- Refuse the entry, not the document: a bad command or hook declaration is
  skipped with a toast and its siblings load (the scoped-refusal rule).
- Four tests per capability: the empty case, an identity, an independently
  checkable value, a control, each shown red once.

## Dependency order

1. blue: `hosted` (load into a host interpreter, call a definition). Shipped
   first, in blue, because everything else stands on it.
2. arnes: `arnes-blue` crate, the host type and the `arnes_*` primitives.
3. arnes-config: `CommandDecl`, `ToolDecl` (rung 1), `HookDecl` (rung 2).
4. arnes-tui: user commands in the `/` picker and in typed `/name` dispatch.
5. arnes: command dispatch; blue tools in `session_tools` and the gate.
6. Hooks.
7. vigy: add the host selector to vigy-runtime; then arnes embeds the runtime
   on the shared store, the `arnes` extension, blue-authored vigies, and the
   MCP health surface.
8. Segments and keybindings (segments can read vigy results).
