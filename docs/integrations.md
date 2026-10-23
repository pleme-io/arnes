# Fleet integrations — what arnes reuses instead of building

Status: living document, started 2026-09-26.

This page holds the survey of pleme-io primitives arnes should consume,
ranked so each one builds on the one before. Each is integrated one at a
time: tested, verified live where possible, and committed before the next
starts.

**The rule** (operator, 2026-09-26): *"re-use and redistribution … we
should basically always do things this way for the compounding effort"*.
In practice:

- A gap in a shared primitive is fixed in that primitive, so every
  consumer gets the fix.
- A second consumer of a primitive is the signal to lift it into its own
  crate.

## Done

| # | Primitive | Replaces | Receipt |
|---|---|---|---|
| — | `egaku::TextArea` readline editing (kill ring, word motion, undo), upstreamed | a one-line `String` prompt | egaku `b1c8d61` (0.1.16), arnes `44cb49a` |
| — | escriba's vim vocabulary + `unsoku` resolver | re-deriving vim | arnes `5ac6765` (the host); egaku `19b7401` row access (0.1.17) |
| — | escriba's key pipeline, extracted from escriba-runtime into `escriba_keymap::KeyPipeline` | a third copy of vim's key layer | escriba `3be8e59` (+ Esc-cancels-operator fix `c7c3f29`), 0.1.85 |
| 1 | `rmcp` client (umbra / mado pattern; rmcp 0.15 like kaname / mado) | the `arnes-mcp-client` stub | arnes `26caf8c`; live against `tend mcp` 6/6 |
| 2 | blackmatter **anvil** (`serversForScope`) + `shibori` for deferred tool search | a hand-written server list; 800 inline schemas | arnes `70e70bb`; 19 servers, first request 359,363 → 1,003 bytes |
| 3 | `egaku::Modal` + `egaku_term::draw::modal_with` — permission dialog | nothing (tools ran unasked) | arnes `cd94dff`; live 6/6 |
| 4 | `mojiban` markdown + highlighting (code blocks fixed upstream, 0.1.3) + `egaku::TextView` wrapping | plain `wrap_text` of raw markdown | mojiban `a07f89e`, arnes `0bbc9dd` |
| 5 | escriba `KeyPipeline` + `escriba-command::ex::parse` → vim in the prompt | vim navigation, `:q` / `:w` | arnes `b7f4131`, live 7/7; escriba's awase floor fixed `30dd7c8`; fleet default `editorMode = "vim"` (nix `modules/shared/arnes.nix`) |
| 8 | mado's OSC builders **lifted into `egaku::vt`** (0.1.18); mado re-exports them | notifications, window title | egaku `fdc1cd9`, mado `6e33447`, arnes `8fbced3`; on the wire 9/9 |
| 7 | `egaku::FuzzyPicker` + new `egaku_term::draw::picker_with` (0.3.18, upstreamed) + the `ignore` walker | `@` file mentions (contents attached on submit), `/` commands (clear, exit, help, mcp, mode, vim) | egaku-term `c43f495`; arnes (this commit); live 10/10 |
| 9 | `shikumi::ConfigStore::load_and_watch` | config hot reload | arnes `b36c4b4`; live 6/6 |
| 12 | `forge-core` cost `Event` (the *-forge wire format) + DuckDB queries | the usage ledger (token plan Phase 1 / §4.3) | arnes (this commit); live 4/4, and `docs/queries/usage.sql` runs on the real file |
| 11 | `cofre-secret` — the API key is a `Secret` (redacted `Debug`, cannot be formatted, scrubbed on drop), `Arc`-shared across turn clones | the key printed by any `{:?}` of the client | arnes (this commit); `expose()` appears at exactly the 3 HTTP-header sites; live catalogue fetch with the real key |
| 6 | egaku `chigai` — **`unified()` added upstream** (0.1.19; lift trigger fired, arnes is the 2nd consumer) | coloured diffs for the new built-in Edit/Write tools | egaku `1becf69`; arnes built-in tools (this commit), live 6/6 |
| 14 | `wadachi_spec::usage` — **lifted out of skim-tab's `usage_rank`** (the shell `Ctrl-R`'s frecency and pin-newest), ranking arnes's prompt history in the shared `egaku::FuzzyPicker` | a substring reverse-i-search over this session only (kept as `history.search = incremental`) | wadachi `59d891b`, skim-tab `b82def8`; arnes (this commit) |
| 10 | `guardrail` — **`production_engine()` lifted into its library upstream**, so arnes screens with exactly the Claude Code hook's rule set | Bash commands and Write/Edit content screened before the permission prompt | guardrail `87ae094`; arnes (this commit); live 5/5 in bypass mode |

## Next, in order

| # | Primitive | Gap it closes | Notes |
|---|---|---|---|
| 13 | `zenmai` + `tatara-lisp-eval` | turn / permission machine; Lisp hooks and custom commands | |

**No pleme-io primitive exists** (build it in arnes, then lift it once a
second consumer appears):

- the SSE streaming client;
- tokenizers (use the provider's usage fields instead);
- search-replace edit and bash-with-timeout tools — now built in `arnes-tools` (Read, Write, Edit, Glob, Grep, Bash, shaped like Claude Code's); they lift to a shared crate when a second consumer appears;
- a small gitignore walker (`codesearch::FileWalker` sits inside a heavy
  crate).

## Findings the integrations surfaced

- **Silent failures, all fixed at the source:**
  - shikumi was built without its `lisp` feature;
  - arnes-config used camelCase keys while shikumi emits snake_case;
  - repo-forge rendered option names as config keys;
  - mojiban dropped code blocks.

  Each silently dropped configuration or content, and none raised an
  error.
- **Vacuous tests that let those bugs through:** mojiban's code-block
  tests ("content present in some form"), and an arnes fixture built from
  a DuckDB-flattened view of an API response.
- **Testing trap:** `cargo test` does not rebuild `target/debug/<bin>`. An
  end-to-end run against the binary must `cargo build` first, or it tests
  the previous build.
- **Live turn progress:** tool calls and results appear only when a whole
  turn ends, because the turn works on a snapshot. Claude Code streams them.
  This is the channel-driven turn-events work in token plan Phase 4.
- **postgres MCP:** in the fleet config it fails its handshake, both in
  Claude Code and in arnes.
- **Dependency floor:** a floor declared too loose breaks consumers, not
  the crate itself. escriba declared `awase = "0.1"` while its keymap used
  `awase::Reserved` from 0.1.8, so arnes (with awase 0.1.7 locked) failed
  to compile escriba-keymap. The floor must name the first release that has
  what the crate uses.
- **Vacuous test filters:** a cargo filter that matches nothing reports "0
  passed" and exit 0. Read the count, not the exit code.
