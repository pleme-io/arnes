# TUI parity with Claude Code — plan

Status: **T0 realized** (2026-09-26); T1–T5 proposed.

**T0 shipped:**
- the keymap as data (`arnes-tui/src/keymap.rs`, Claude Code chord syntax
  parsed by awase);
- the input reducer (`arnes-tui/src/input.rs`);
- readline editing, upstream in `egaku::TextArea` 0.1.16;
- a turn task that can be interrupted;
- bracketed paste with chips;
- queueing, history, `Ctrl-R`, stash, `$EDITOR`, `Alt-T` effort cycling,
  the `?` help panel, and a tail-anchored conversation.

**Verification:**
- 35 arnes-tui tests (keymap, reducer, render snapshots);
- 12 egaku readline tests;
- a pseudo-terminal run against a mock provider (9/9), asserted on a
  VT-emulated screen.

The pseudo-terminal harness is a scratchpad script today. Its committed
Rust form is the PTY tier in §6.

**Owed from T0:**
- `Ctrl-Z` suspend;
- moving bracketed paste and Kitty keyboard-protocol setup from arnes's
  `Protocols` guard into `egaku_term::Terminal` options.

## 1. Destination

In the terminal, arnes behaves like Claude Code: the same keys, editing,
rendering, sessions, headless mode, permissions and customization. The
model is the only deliberate difference (DeepSeek by default).

A terminal developer who knows Claude Code should never have to learn
anything new. Every behaviour below is one of two things: a row in a
parity matrix a test enforces, or a *named, written-down* divergence.

Parity is built on the fleet TUI stack and never beside it. Each gap is
filled in the shared layer, so every other fleet TUI gets it too:

| Library | Owns |
|---|---|
| `egaku` | Editing primitives |
| `egaku-term` | The terminal runtime, including the inline screen mode |
| `awase` | Typed keys and chords |
| `moldura` | The app frame |

## 2. Reference

The Claude Code behaviour was taken from the official docs on 2026-09-26:

- <https://code.claude.com/docs/en/interactive-mode.md>
- <https://code.claude.com/docs/en/keybindings.md>
- <https://code.claude.com/docs/en/terminal-config.md>
- <https://code.claude.com/docs/en/fullscreen.md>
- <https://code.claude.com/docs/en/sessions.md>
- <https://code.claude.com/docs/en/headless.md>
- <https://code.claude.com/docs/en/permission-modes>
- <https://code.claude.com/docs/en/statusline>

Where the docs were silent or ambiguous, the row says **unconfirmed**. Such
a row is verified against a running Claude Code before it becomes a test.

## 3. Where arnes stands (2026-09-26)

- **Keys:** only four are handled (`arnes/src/main.rs`).
  - `Esc` **quits**; in Claude Code it interrupts the turn.
  - `Ctrl-C` is not handled. The printable-character arm ignores
    modifiers, so it probably inserts a literal `c`.
- **Input:** one line, no editing keys, no history, no paste handling.
- **Screen:** alternate screen (`egaku-term::Terminal`), so the terminal's
  own scrollback is lost. The view draws from the first message, so new
  output falls off-screen, and nothing scrolls.
- **Interrupting:** impossible. The turn holds the state lock across the
  network call, so no key is seen until the call returns.
- **Missing entirely:** sessions, headless mode, permissions, slash
  commands and `@` mentions.

## 4. Parity matrix

Each area lists its behaviours, the owning layer, and the phase that ships
it.

### 4.1 Global keys (T0)

| Key | Claude Code behaviour | arnes action |
|---|---|---|
| `Ctrl-C` | With text: clear the input. During a turn: interrupt it. Otherwise arm exit; a second press exits. | `app:interrupt` / `chat:clearInput` / `app:exit` |
| `Ctrl-D` | With text: delete forward. Empty: a hint, then exit on a second press within 800 ms. | `chat:deleteForward` / `app:exit` |
| `Esc` | Interrupt the running turn, keeping partial work; close a dialog. | `app:interrupt` |
| `Esc Esc` | With text: clear it into history as a draft. Empty: open rewind (T2). | `chat:clearToHistory` / `app:rewind` |
| `Enter` | Submit. During a turn: queue the message. | `chat:submit` |
| `Shift-Enter`, `Alt-Enter`, `\`+`Enter`, `Ctrl-J` | Insert a newline. | `chat:newline` |
| `Up`/`Down`, `Ctrl-P`/`Ctrl-N` | Move within multi-line input; at the first or last row, walk history. | `history:previous/next` |
| `Ctrl-R` | Reverse history search: the ranked fuzzy picker (`history.search = picker`, the fleet's shell `Ctrl-R` ranking) or reverse-i-search (`incremental`). History persists across sessions (`history.*`). | `history:search` |
| `Ctrl-L` | Full redraw, keeping input and history. | `app:redraw` |
| `Ctrl-O` | Toggle the transcript / verbose view (T1). | `app:toggleTranscript` |
| `Ctrl-G`, `Ctrl-X Ctrl-E` | Edit the prompt in `$VISUAL`/`$EDITOR`. | `chat:externalEditor` |
| `Ctrl-S` | Stash the draft; press again to restore it. | `chat:stash` |
| `Ctrl-Z` | Suspend (Unix). | `app:suspend` |
| `Shift-Tab` | Cycle permission mode (T4). | `app:cycleMode` |
| `Alt-P` / `Alt-T` | Model picker / thinking toggle. For arnes this cycles `effort` off → low → high → max. | `chat:modelPicker` / `chat:thinkingToggle` |
| `?` on empty input | Shortcut help panel. | `app:help` |

### 4.2 Readline editing (T0, in `egaku::TextArea`)

- **Movement:** `Ctrl-A` / `Ctrl-E` (start / end of the logical line),
  `Ctrl-B` / `Ctrl-F`, `Alt-B` / `Alt-F` (word motion).
- **Kill and yank:**
  - `Ctrl-K` kills to the end of the line; `Ctrl-U` kills to the start
    (and repeats across lines).
  - `Ctrl-W` kills back to whitespace, so one press removes a whole path
    or flag.
  - `Alt-D` kills forward to the end of the word.
  - All of these feed a **kill ring**: `Ctrl-Y` yanks the last kill, and
    `Alt-Y` cycles older ones.
- **Delete:** `Ctrl-H` and `Backspace` delete back; `Delete` deletes
  forward.
- **Undo:** `Ctrl-_` restores the text and the cursor.

### 4.3 Input surface (T0–T4)

- **Bracketed paste.** A paste over the collapse threshold becomes a
  `[Pasted text #N]` chip, and the real text is sent (T0). The threshold
  is **unconfirmed**: the docs say roughly 1 KB.
- **Prefixes and pickers:**
  - `/` slash commands with autocomplete (T2 built-ins, T4 custom `.md`)
  - `@` file mention picker (T4)
  - `!` bash mode (T4)
  - `#` memory (T4)
- **Queueing.** Typing during a turn queues the message and shows
  "Queued: N". `Up` from the first row takes a queued message back.
  `Ctrl-Enter` sends now (T0 queues; T1 shows the count).

### 4.4 Rendering (T1)

- **Inline mode (the defining trait).** Finished messages are written
  once into the terminal's normal scrollback. Only a small live region at
  the bottom (spinner, input, status line) is redrawn. The terminal's own
  scrollback, search and copy keep working.
  - This needs `egaku-term::Terminal::inline(height)` (a viewport pinned
    to the bottom, no alternate screen) **upstream**.
  - The alternate screen stays available as a setting: `screen = "inline" | "fullscreen"`.
- **Messages:** Markdown with syntax-highlighted code blocks.
- **Tool calls:** the name plus a one-line input summary.
- **Tool results:** collapsed to a line count, expandable in the
  transcript.
- **Edits:** coloured, line-numbered diffs.
- **Spinner:** elapsed time plus streamed-token counters, the "thinking"
  state, and a reduced-motion setting.
- **Status line:** model, effort, permission mode, context used, cache-hit
  ratio and cost (the ledger from `token-efficiency.md` §4.3), plus the
  queued count.
- A `statusLine.command` hook takes JSON on stdin (T5).

### 4.5 Sessions (T2)

- **Storage:** transcripts are JSONL at
  `$XDG_DATA_HOME/arnes/sessions/<project>/<id>.jsonl`.
- **Resuming:** `--continue` reopens the latest session; `--resume`
  opens a picker, and `--resume <id|name>` opens one directly.
- **Commands:** `/clear`, `/compact`, `/context`, `/cost`, `/model`,
  `/export`, `/rename`, `/branch`, `/rewind`, `/help`.
- **Checkpoints:** a file snapshot before each edit tool runs (T4, once
  tools can edit).

### 4.6 Headless (T3)

- `-p` / `--print` reads the prompt from arguments or stdin (capped at
  10 MB).
- `--output-format text | json | stream-json` and `--input-format`.
- `--max-turns`, `--append-system-prompt`, `--allowed-tools`,
  `--permission-mode`.
- Exit codes: 0 success, 1 error, 130 SIGINT, 143 SIGTERM.

### 4.7 Permissions and tools (T4)

- **Modes:** default, acceptEdits, plan and bypass. `Shift-Tab` cycles
  them.
- **Approval prompt:** "yes", "yes, and don't ask again for …" and "no,
  tell arnes what to do instead". `Up`/`Down`, `Enter`, `Esc` and `Tab`
  (comment) work inside it.
- **Rules:** allow/deny rules use the `Tool(pattern)` syntax, typed in
  tatara-lisp.
- **MCP tools:** wired, with order frozen per `token-efficiency.md` §4.2.
- **Memory files:** `ARNES.md` at user, project and local levels. arnes
  also reads `AGENTS.md`, and `CLAUDE.md` behind a setting.
- **Hooks:** `PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`.
  The exact exit-code contract is **unconfirmed** in the docs and gets
  verified before it is implemented.

### 4.8 Customization and terminal integration (T5)

- **Keybindings.** One typed keymap: contexts (Global, Chat, Autocomplete,
  Confirmation, HistorySearch, Transcript, …) × chord → action.
  - Authored in tatara-lisp (`(defkeybindings …)`) or through the HM
    option, with `null` meaning unbind.
  - `Ctrl-C`/`Ctrl-D` double-press exits are reserved and cannot be
    rebound, as in Claude Code.
  - The default keymap *is* §4.1–4.2, as data.
- **Terminal integration:**
  - themes, `NO_COLOR`
  - notifications (bell / OSC 9) when a long turn or a permission prompt
    needs attention
  - window title
  - vim editor mode
  - OSC 8 hyperlinks
  - enabling the Kitty keyboard protocol, so `Shift-Enter` and
    `Ctrl-Enter` are distinguishable

### 4.8.1 Vim — first-class, not a late option

The operator wants arnes to feel like an extension of vim: escriba,
neovim / blnvim, and the vi-modes of blzsh / frost / frostmourne. Claude
Code has a vim editor mode (`/vim`); arnes goes further.

- **Modal prompt editing:** normal / insert / visual modes, with motions,
  operators, counts and text objects over the same `TextArea`.
- **Ex commands:** `:q` / `:q!` / `:wq` map onto exit and submit; `:w`
  submits.
- **The operator's conventions**, read from their own vim and shell
  configs.
- **Mode-aware cursor shape**, as the shells do.

**Decision (recon 2026-09-26): `unsoku` owns vim over a buffer.**

- It lives in the escriba workspace and is on crates.io at 0.1.84.
- It was built for prompt lines, resolves `escriba-core`'s `Motion` /
  `Operator` / `TextObject`, and carries eight named, tested vim rules
  (`cw`≠`ce`, inclusive `de`/`d$`, `xp` register fill, and so on).
- Composition (`{count}{op}{count}{motion}`) is `escriba-mode::OperatorPending`.
- Ex aliases (`w`, `q`, `q!`, `wq`, `x`) come from `escriba-command`.
- arnes adapts `egaku::TextArea` through unsoku's `TextTarget`, so egaku
  never learns vim.

**Owed upstream, in order:**

1. **egaku.** Row-level access so a `TextTarget` can sit over the current
   row: current row text, caret as a byte offset, and replace-in-row with
   an undo checkpoint. Plus linewise row operations (delete, insert
   above/below) for `dd` / `yy` / `p` / `o` / `O`.
2. **unsoku.** Multiple rows: a multi-line target, `j`/`k`/`gg`/`G`,
   paragraphs, linewise registers. Visual and VisualLine stances. A link
   from its register to `TextArea`'s kill ring.
3. **The mode boundary.** An escape-sequence timeout (`jk` style), taken
   from the operator's `KEYTIMEOUT=20`, i.e. 200 ms. A cursor-shape signal
   using escriba's `CursorShape`: steady block in normal, steady bar in
   insert, matching blzsh and frost-zle. `:q` / `:q!` / `:wq` / `:w` routed
   into the reducer's exit and submit effects.

**The operator's conventions to honour** (from blackmatter-shell and
blackmatter-nvim):

- In vicmd, `k`/`j` walk history, `/` and `?` search history, and `v`
  opens the prompt in `$EDITOR`.
- `Ctrl-A` / `Ctrl-E` work in both modes.
- nvim uses leader `,` and `timeoutlen` 300. No `jk` escape is bound
  today, so it is opt-in.

**One implementation, not three.** frost-zle uses reedline's own Vi mode,
and escriba-runtime still resolves motions against its rope. Moving both
onto a multi-row unsoku is the long-term destination. arnes is the first
consumer outside escriba.

### 4.9 Named divergences

| Divergence | Why |
|---|---|
| Default model DeepSeek; `Alt-T` cycles effort rather than toggling thinking | The provider's own controls (`token-efficiency.md` §4.1) |
| No voice input, fast mode, or cloud features | Out of scope for a local harness |

## 5. Architecture

- **A pure reducer.** `TuiState::apply(Action) -> Vec<Effect>` holds all
  input semantics with no I/O. Keys resolve to `Action`s through the
  keymap. Effects (submit, interrupt, exit, redraw, open the editor) are
  carried out by the runtime.
  - Every row of §4.1 becomes a reducer test that needs no terminal.
- **A turn task with a cancellation handle.** The turn runs detached, and
  its events (deltas, tool calls, usage, done) arrive on a channel.
  - `app:interrupt` cancels it and keeps whatever was already received.
  - This is the same change as `token-efficiency.md` Phase 4, and they
    ship together.
- **Keymap as data.** The keymap lives in `egaku::KeyMap<A>`, with contexts
  and `awase` chords (`ctrl+x ctrl+e`).
  - `egaku-term::event::testing::assert_all_deliverable` proves each
    default binding can actually reach the app under the enabled terminal
    protocol.
  - That matters for `Shift-Enter`, which needs the Kitty keyboard
    protocol and has `\`+`Enter` / `Ctrl-J` as fallbacks.

## 6. Test plan

| Tier | Test | Negative control |
|---|---|---|
| Parity matrix | One row per §4.1/§4.2 binding: the key maps to its action in the default keymap, and the reducer produces the documented effect. The build fails when an `Action` variant has no row. | Delete a binding → red |
| Reducer | Scripted key sequences: `Ctrl-C` with text clears; two `Ctrl-C`s exit; `Esc` during a turn emits `Interrupt` and keeps partial output; `Up` at row 0 walks history; paste over the threshold makes a chip that submits the real text. | Remove the 800 ms window → the timing test fails |
| Editing (in egaku) | Kill ring, yank, yank-pop, word motion over paths/flags, undo restoring the cursor. | — |
| Deliverability | `assert_all_deliverable` over the default keymap for each terminal protocol level. | Bind `Ctrl-Shift-letter` without Kitty → red |
| Render snapshots | `egaku-term` `Buffer` goldens: collapsed tool result, diff, status line, spinner. | — |
| PTY end-to-end | Spawn `arnes` in a pseudo-terminal against the mock provider (`token-efficiency.md` §5), send real key bytes, assert the screen text and exit status. | — |
| Headless | `-p` with `--output-format json` / `stream-json` goldens against the mock provider; exit codes. | — |

## 7. Phases

| Phase | Scope | Exit criterion |
|---|---|---|
| **T0 — Input correctness** | Reducer + keymap. §4.1 core keys (`Ctrl-C`/`Ctrl-D`/`Esc`/`Enter`/newlines/history/`Ctrl-L`/`Ctrl-S`/`Ctrl-G`). §4.2 readline, upstream in egaku. Bracketed paste and chips. Queueing. Interruptible turn task. | Parity-matrix and reducer tiers green |
| **T1 — Rendering** | `egaku-term` inline mode upstream; markdown/code/diff/tool rendering; spinner; status line with ledger; `Ctrl-O` transcript | Snapshot tier green; scrollback survives exit |
| **T2 — Sessions** | JSONL transcripts, `--continue`/`--resume`, built-in slash commands | Resume round-trip test |
| **T3 — Headless** | `-p`, output formats, stdin, exit codes | Headless goldens |
| **T4 — Permissions and tools** | Modes, prompts, rules, MCP wiring, `@`, `!`, memory files, hooks, checkpoints | Permission reducer tier + PTY end-to-end |
| **T5 — Customization** | `defkeybindings`, themes, notifications, title, vim mode | Parity matrix covers custom rebinding and reserved keys |
