# arnes screen law

Short on purpose. The code is `arnes-tui/src/layout.rs` (the grid),
`chrome.rs` (everything drawn in it except the conversation's content) and
`look.rs` (theme, the declared knobs, motion). Tests:
`arnes-tui/tests/chrome_gallery.rs`.

## 1. One grid, reserved regions

The screen is one column of regions, stacked top to bottom:

```
conversation     flexible; help, /mcp and panels replace it in place
claim            a picker or the approval question, when open
notices          one row per live notice, 0..notice-rows
activity         1 row: what is happening now
prompt           3..10 rows, grows with its text
status           1 row: mode, model, effort, tools, permission, spend
```

Regions tile the screen and never share a cell. Nothing is drawn over
anything else. A surface that comes and goes takes rows from the
conversation, which reflows above it and keeps its newest line visible.
So pickers and the approval question claim space. They are not modals, and
nothing is dimmed.

On a small terminal, rows go out in this priority order: the prompt box, the
status line, the activity line, one conversation row, notices, the claimed
surface, the prompt's extra rows, then the rest to the conversation.

## 2. Every notice has a home

Errors, warnings and info go into one queue with a severity, and they show
in the notice lane under the conversation. Each notice sits behind an edge
in its severity colour, slides in along its own row, and fades toward muted
over the last 30% of its life. A repeat folds into the notice that is
already showing (`×3`). When more notices are live than the lane can show,
the top row says `+N more · /notices`. `/notices` reviews the session's log,
newest first. Nothing writes to the terminal behind the TUI; the details go
to the log file.

## 3. Always say what is happening

The activity line is never silent during work:

- `⠋ thinking 3.2s`, with a shimmer on the word
- `⠋ query on postgres 1.3s +1`: the running tool, its MCP server, how many
  others are running
- `◆ waiting on you · Bash`: an approval is open
- `✓ Read 0.4s` on the right: the last tool that finished, lit when it
  arrives and settling to muted
- `answered by gpubox (fallback)`, `allowed Bash`, `denied Bash`: short notes

When idle it is calm: `ready · local llama.cpp · free`.

## 4. Acknowledge every change, quietly

Every state change is acknowledged for the fleet's fast duration (150 ms):
a mode switch (INSERT/NORMAL), the permission mode, effort, a model or
route switch, a new hint (copied, attached, config reloaded), a sent prompt
(the prompt border pulses), a picker or approval selection moving (the
selection band eases in), a list scrolling (its position lights up, then
settles), a tool tick, and an approval answer.

An acknowledgement only changes colour. It never shifts or covers anything.
With `:motion "off"`, everything is drawn at its end state.

## 5. Minimal

Every glyph carries information. There are no boxes for their own sake, no
decorative icons, and no emoji. Colours come from the fleet theme that
`:tema` names. Motion comes from ishou's curves and durations, through
`egaku_term::anim`.

## 6. Works with no config

With no config and no key, arnes starts on the free route: the local
llama.cpp server, then, on macOS, Apple's on-device model. The activity
line says which route is in use. A paid provider is used only once you give
it a key, so it is always something you opt into.

## 7. Declared, not coded

These knobs are shikumi config (tatara-lisp, YAML or Nix). A bad value is
refused on its own, named in a notice, and the built-in value is kept. The
full surface, including the `ui`, `activity` and `keys` sections these flat
keys now sit beside, is `docs/configuration.md`.

| key                                  | default    |
|--------------------------------------|------------|
| `:tema`                              | pleme_dark |
| `:motion`                            | on         |
| `:notice-rows`                       | 3          |
| `:notice-info-secs` / `-warn` / `-error` | 4 / 8 / 12 |
| `:picker-rows`                       | 12         |

Rust holds only the engine: the layout, the drawers and the clock.
