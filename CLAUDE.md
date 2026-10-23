# Arnes

> **★★★ CSE / Knowable Construction.** This repo operates under **Constructive Substrate Engineering** — canonical specification at [`pleme-io/theory/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md`](https://github.com/pleme-io/theory/blob/main/CONSTRUCTIVE-SUBSTRATE-ENGINEERING.md). The Compounding Directive (operational rules: solve once, load-bearing fixes only, idiom-first, models stay current, direction beats velocity) is in the org-level pleme-io/CLAUDE.md ★★★ section. Read both before non-trivial changes.

> Rust/tatara-lisp open-source Claude-Code-equivalent agent harness

## Blue is the primary way to implement anything in arnes

Operator, 2026-10-08: *"arnes should be as much bluelang as possible with as
little Rust as possible."* Rust in arnes may shrink or hold, never grow
silently: `kernel-baseline.txt` records each crate's Rust line count and the
host-function count with the world-fact that keeps it Rust (tty, network,
secret, an invariant a bidama must not break), and the `kernel-ratchet`
generator (`gen/kernel-ratchet.b`, over the `hadome` bidama) fails when a
measurement passes its row. The arnes package depends on
`checks.generated-kernel-ratchet-fresh`, so a rebuild cannot install an arnes
whose Rust grew unrecorded or whose `docs/kernel-ratchet.txt` is stale
(`nix run .#regen` refreshes it). Growing Rust is an edit to that file plus a
`docs/kernel.md` §1 row, in the same commit. A new
host function is generic and capability-scoped; the policy that calls it is a
bidama. Before adding Rust, name the bidama that cannot be written.

Operator, 2026-09-28: arnes is shikumi-programmable, and that means blue first.
New behaviour (slash commands, hooks, tools the model can call, status-line
segments, keybindings) is written in blue (`.b`) and declared in the
shikumi config (`(defarnes …)` tatara-lisp, or YAML/Nix). Walk the ladder
before writing: blue, then tatara-lisp, then Rust only for what neither can
express (the host seam, the TUI engine, a system call). A capability blue lacks
is built into blue (a bidama or runtime primitive), not routed around in Rust.
Live per-tick state (MCP server health first) comes from vigies on the shared
vigy-runtime, authored in blue (`theory/VIGY.md`, `vigy` skill).
Plan and extension points: `docs/programmability.md`. Procedure: the
`pleme-style` and `bidama` skills.

## Arnes is a blue-hackable kernel

Operator, 2026-09-28: everything in arnes is configurable and programmable in
blue, and every feature is a blue extension or module. The modules are bidamas (blue
packages with a Bluefile), and arnes is, in practice, a bidama host. The Rust side is a small
kernel (terminal, render engine, provider transport, MCP client, the blue host
API); built-in commands, panels, status and activity segments, notice policy,
keymaps, routes and tools ship as a default blue module set a user can replace,
extend or turn off. A feature that exists only in Rust is a gap to close. Plan:
`docs/kernel.md`. arnes's bidamas live in `bidamas/` (a `NAMES.md` row each);
before writing one, read blue's `bidamas/CATALOG.md` and compose what exists,
and move a shared shape into the public bidama rather than copying it.

## Developing arnes

Procedure: the `arnes` skill. The rules it enforces, briefly:

- **Reuse, then lift.** Look in the shared crates first (egaku widgets, wrap
  and navigation; egaku-term palette and `anim`; mojiban `render_markdown`;
  ishou-tokens theme and motion; vigy; shikumi). A primitive arnes needs that
  another app could use goes into that crate and ships through its
  auto-release, then arnes consumes it. arnes keeps only what is arnes.
- **Screen law** (`docs/design.md`): one grid of reserved regions, nothing
  drawn over content; notices live in their own lane; an activity line always
  says what is happening; every state change gets a subtle motion-token
  acknowledgement that honours reduced motion; no decoration without
  information.
- **One navigation.** Every list, menu and panel uses the shared vim navigation
  component and keymap-rendered footer hints. No per-screen key handling.
- **Works by default.** With no config or key, arnes runs on the free or local
  route and says so; paid routes are opt-in; every knob is shikumi config.
- **Every knob is declared** in `docs/configuration.md`: default, bound, and
  whether it applies live. A new knob is a field in `arnes-config` (a section
  via `section!`), a typed leaf in `module/default.nix` (a test fails
  without it), and a row in that doc. A bad value is refused alone; a config
  change applies in place, never by restart.
- **Live data is vigy**, persistence and caches follow `docs/persistence.md`.
- **Proof is offscreen.** Render tests on an egaku-term `Buffer`
  (`ARNES_GALLERY=1` prints them) at several sizes, animation tested at pinned
  instants, never with sleeps. After a dependency change, `gen build` so
  `Cargo.lock` and `Cargo.gen.lock` agree.
