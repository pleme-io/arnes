# Token, cache and memory efficiency — research and plan

Status: **Proposed** (2026-09-26). Nothing below is implemented yet except
where a phase says so.

## 1. Destination

arnes is a **cache-first** harness. Every request is a byte-stable
extension of the previous one. The cache hit ratio and the cost of every
turn are measured and visible, not assumed. Context stays bounded by a
typed policy that only ever breaks the cache when the savings pay for the
break. Cloning, locking and re-rendering cost O(new content) per turn, not
O(history).

"Done" means three things:

- a CI simulation proves the prefix stays stable for every request shape
  arnes can build, with a negative control that makes the gate go red;
- a live benchmark reports the hit ratio and cost per task from DeepSeek's
  own usage numbers;
- a new provider or model is a row of data, not new code.

Why cache-first: on `deepseek-flash`, a cache hit costs **2%** of a miss
($0.006 vs $0.30 per 1M tokens at peak), and agent traffic is about
**100:1** input to output. The hit ratio outweighs every other lever
combined. Moving from a 50% to a 95% hit ratio cuts input cost about 7×.

## 2. Where arnes stands (audit, 2026-09-26, commit `76b0652`)

Each receipt below is a `file:line` in this repo.

| # | Finding | Receipt | Effect |
|---|---|---|---|
| A1 | Default model `deepseek-chat` was retired. The live `/models` lists only `deepseek-flash` and `deepseek-v4-pro`, and unknown names are silently routed to flash with thinking **on/high**. | `arnes-config/src/lib.rs` `Provider::default_model` | You pay for reasoning you never asked for |
| A2 | Thinking is stripped from history for DeepSeek. The current docs require it to be sent back whenever `tools` are sent, or the API returns 400, and stripping also breaks the cached "end of model output" units. | `Provider::echoes_thinking`, `strip_thinking` | 400s once tools are wired; lost cache hits |
| A3 | Only `thinking: {enabled, budget_tokens}` can be sent. DeepSeek ignores `budget_tokens`; the knobs it honours are `thinking: disabled` and `output_config.effort` (low/high/max). | `arnes-anthropic/src/lib.rs` `ThinkingParam` | Output spend can't be controlled |
| A4 | No `cache_control` anywhere, and `system` is a bare string, so nothing is ever cached on Anthropic. | `arnes-core/src/conversation.rs:36` | Anthropic pays full price for the prefix every turn |
| A5 | `body()` clones the whole history, system prompt and tool list per request. The retry loop re-serializes the body on every attempt. | `arnes-anthropic/src/lib.rs` `body`, `send` | O(history) allocations per turn |
| A6 | Tool results enter history verbatim, with no cap or offloading. | `arnes-loop/src/lib.rs:74-84` | One large output is re-billed on every later turn |
| A7 | No context guard. `StopReason` has no fallback arm, so an unexpected stop reason fails to parse. A context-limit 400 is not retried and wedges the session. | `arnes-core/src/message.rs` `StopReason`, `main.rs:195` | Sessions die at the context limit |
| A8 | `Usage`, including the cache fields, is added up and then thrown away (`let _ =`), and `total()` leaves out cache tokens. | `main.rs:119`, `usage.rs:17` | There's no way to see the hit ratio |
| A9 | The TUI holds the state lock across the whole network call and redraws every 100 ms, re-wrapping all visible text. | `main.rs:186-194`, `main.rs:225` | UI freezes; wasted CPU |
| A10 | MCP `advertised_tools()` is a stub, so no tools are sent. The prefix is stable today only because it's empty. | `arnes-mcp-client/src/lib.rs:173` | Tool order will break caching the day tools are wired |
| A11 | `max_tokens` has three disagreeing defaults (8192 / 4096 / 4096). | config `:133`, core `:53`, anthropic `body` | Configured value may not be the one sent |

Probed facts, not docs: `GET https://api.deepseek.com/models` (2026-09-26)
returns `deepseek-flash` (V4.1-Flash, 1,048,576 context, 393,216 max
output, text+image, effort `[low, high, max]` default `high`,
`system_prompt_update: in-history`) and `deepseek-v4-pro` (text only,
`system_prompt_update: leading-only`). The account currently answers
**402 Insufficient Balance**, so no live usage numbers exist yet. Phase 0
cannot finish its live gate until the account is funded.

## 3. Research summary (sources)

### DeepSeek (default provider)

- **Caching is automatic, on disk, best-effort.** A request hits only if
  it *fully matches* a persisted prefix unit. Units are written at the end
  of each user input and each model output, at detected common prefixes,
  and at fixed intervals for long content. Caches build in seconds and are
  evicted after hours to days. <https://api-docs.deepseek.com/guides/kv_cache>
  (The older "64-token unit" rule is from the 2024 launch post; the current
  page states no unit size.)
- **Price per 1M tokens, peak / off-peak.** flash: hit $0.006 / $0.003,
  miss $0.30 / $0.15, output $1.20 / $0.60. pro: hit $0.044 / $0.022,
  miss $1.32 / $0.66, output $3.96 / $1.98. Off-peak means outside
  01–04 and 06–10 UTC on weekdays, plus all weekend.
  <https://api-docs.deepseek.com/quick_start/pricing>
- **Anthropic-compatible endpoint.**
  - Ignored: `cache_control`, `top_k`, `anthropic-version`/`-beta`,
    `is_error`, `disable_parallel_tool_use`.
  - Restricted: `top_p` works only in thinking mode (floor 0.95), and
    `temperature` has no effect in thinking mode. `metadata` accepts only
    `user_id`, and that value partitions the cache.
  - Not supported: `document`, `redacted_thinking`, `search_result`.
  - <https://api-docs.deepseek.com/guides/anthropic_api>
- **Thinking.** On by default at effort high. Without tools, prior
  reasoning is ignored. With tools, all prior reasoning **must** be sent
  back, or the API returns 400.
  <https://api-docs.deepseek.com/guides/thinking_mode> (The page documents
  this for the OpenAI format; that it also applies to Anthropic-format
  `thinking` blocks is an inference, to verify live.)
- **Usage reporting on the Anthropic endpoint is undocumented.** A
  third-party report says it returns `cache_read_input_tokens` and always
  returns `cache_creation_input_tokens: 0`. Record every field raw and
  derive nothing until it is measured.

### Anthropic (secondary provider)

- At most 4 `cache_control` breakpoints. A top-level `cache_control` gives
  an automatic breakpoint that moves forward with the conversation.
  - Each breakpoint looks back 20 blocks.
  - Order is tools → system → messages, and changing a level invalidates
    it and everything after it.
  - Writes cost 1.25× base input (5-minute TTL) or 2× (1-hour TTL); reads
    cost 0.1×.
  - <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>
- Server-side context editing (`clear_tool_uses_20250919`, with
  `clear_at_least`) and compaction (`compact_20260112`) exist, as does a
  free `count_tokens` endpoint.

### Harness practice

- **Stable prefix.** Manus calls the KV-cache hit rate "the single most
  important metric"; its guidance is to mask tools, never add or remove
  them.
  <https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus>
  On DeepSeek, sorting tools and freezing the environment block raised
  Claude Code's hit rate from 33% to 71%; this is a third-party
  measurement. <https://github.com/jianzhichun/permafrost>
- **Observation masking vs summarization.** Masking all but the last 10
  observations cut cost about 50% at an equal solve rate. Summarization
  made trajectories about 15% longer, and its calls get almost no cache
  reuse. <https://arxiv.org/abs/2508.21433>
- **Offloading large tool output.** Claude Code keeps up to about 30k
  characters of bash output inline; beyond that it passes a file path plus
  a 2k preview. <https://code.claude.com/docs/en/tools-reference>
- **Repo map and edit format.** Aider's repo map fits about 1k tokens, and
  unified-diff edits were about 3× less lazy than whole-file rewrites.
  <https://aider.chat/2023/10/22/repomap.html>,
  <https://aider.chat/2023/12/21/unified-diffs.html>
- **Prompt compression (LLMLingua).** Re-compressing history changes the
  prefix, and a cached DeepSeek prefix already costs almost nothing. It is
  worth considering only for a large *fresh* tool output, compressed once
  when it arrives (inference).

## 4. Design — the typed primitives

Each item extends an existing type; none adds a parallel mechanism.

### 4.1 `ProviderSpec`: capabilities as data (fixes A1, A2, A3)

Today `Provider` hard-codes four facts. The plan widens it into a table
row, `ProviderSpec`, with these fields:

- `endpoint`, `key_env`
- `cache: CacheMode { Automatic, Breakpoints { max: 4 } }`
- `thinking_echo: EchoRule { Never, Always, WhenTools }`
  - DeepSeek becomes `WhenTools`, which fixes A2.
  - Anthropic stays `Always`.
- `ignored_fields`, e.g. DeepSeek ignores `top_k`. arnes warns once instead
  of sending a no-op.
- `effort_levels`
- `default_model`: DeepSeek becomes `deepseek-flash`.
- `prices: PriceTable` with peak and off-peak windows.

**The model catalogue is read, not restated.** `arnes models` calls
`GET /models` and caches the result, with its timestamp, at
`~/.cache/arnes/models.json`. Context window, max output, effort levels
and modalities come from the provider itself. A configured model that
isn't in the catalogue is refused at startup, naming the catalogue. This
closes the silent remap that bit A1.

Thinking becomes one typed field:
`ThinkingMode { Off, On { effort: Effort, budget: Option<u32> } }`, where
`Effort` is `Low | High | Max`.

- It renders to `thinking: {type: disabled}` or
  `thinking: {type: enabled}` plus `output_config.effort`.
- `budget` is only emitted where the spec says it is honoured.
- Per-phase policy (config): the effort for routine tool steps defaults to
  `Low`; the effort after a tool error or a user "think harder" defaults
  to `High`.

### 4.2 Canonical request bytes (fixes A5, guards A10)

- `MessagesRequest<'a>` borrows `&'a [Message]`, `&'a [Tool]` and
  `&'a SystemPrompt`.
- Thinking is stripped by a `Serialize` wrapper that skips blocks while
  writing, never by building a copied `Vec`.
- The body is serialized **once** into `bytes::Bytes` before the retry
  loop, so a retry sends byte-identical data.
- Tools are sorted by `(server, name)` at registration, and the sorted
  list is frozen for the session. A tool list that changes mid-session is
  a typed event that the ledger records as a cache break.
- `SystemPrompt` becomes `Vec<SystemBlock>` so that
  Anthropic can carry a breakpoint.
  - It is static for the session.
  - Anything that changes (cwd, git status, date) is sent as an *appended*
    user-side message, never an edit to the system prompt.
  - This uses the `in-history` capability the flash catalogue advertises.
    The pro catalogue says `leading-only`, and the spec enforces that per
    model.

### 4.3 Usage ledger (fixes A8)

- One `TurnRecord` per API call, holding:
  - request and response ids;
  - the provider's usage fields, **stored raw**;
  - model and effort;
  - bytes sent;
  - a `prefix_hash` (BLAKE3 over the canonical prefix bytes);
  - whether the call was peak or off-peak;
  - computed cost.
- Records are appended to `~/.local/state/arnes/usage.jsonl`.
- The TUI status line shows session tokens, cache-hit ratio and cost.
- Analysis runs in DuckDB over the JSONL. The standing queries live in
  `docs/queries/`, generated by the `bunseki` bidama once it can express
  them.
- `Usage::total()` counts every input class.

### 4.4 Tool-output budget (fixes A6)

- Output larger than `tool_output.inline_max` (default 16 KiB) goes to a
  content-addressed blob, `~/.cache/arnes/blobs/<blake3>`.
- History gets a **stable stub**: `{blob, bytes, lines, head[1k], tail[1k]}`.
- A built-in `read_blob(hash, offset, len)` tool lets the model fetch more.
- The stub is written once and never rewritten, so it doesn't disturb the
  prefix.
- Identical outputs are deduplicated by hash.

### 4.5 Context policy (fixes A7)

A typed `ContextPolicy`, driven by the catalogue's context window:

1. **Mask** once the prompt passes `mask.trigger` (default 60% of the
   window). Every observation older than the newest `mask.keep` (default
   10) is replaced by a one-line stub pointing at its blob.
   - Masking only fires when at least `mask.clear_at_least` tokens
     (default 50k) become cleanable.
   - Every mask breaks the cache from that point on, so it is done rarely
     and in large batches. This is the JetBrains masking result combined
     with Anthropic's `clear_at_least` rule.
2. **Compact** only past `compact.trigger` (default 85%). This means an
   LLM summary of the masked history.
   - Its cost and cache break are recorded in the ledger.
   - On Anthropic, prefer server-side `compact_20260112` when enabled.
3. **Guard.**
   - `StopReason` gains `PauseTurn`, `ModelContextWindowExceeded` and a
     `#[serde(other)] Unknown`.
   - A context-limit 400 triggers one forced mask-then-retry, never a
     wedged session.
   - `MaxTokens` continues the turn, up to `continue.max` times.

### 4.6 Anthropic caching path (fixes A4)

When `CacheMode::Breakpoints`:

- one breakpoint at the end of the static prefix (tools + system);
- top-level automatic caching for the moving tail;
- `cache_ttl: 5m | 1h` as a setting.

Optional server-side `clear_tool_uses` goes behind
`anthropic.context_editing = true`. Nothing in this path is emitted for
DeepSeek, which ignores it.

### 4.7 Memory and the UI (fixes A9)

- History becomes `Vec<Arc<Message>>`, so a snapshot for the UI or a
  spawned turn is a reference-count bump.
- The turn runs on its own task. Progress, streamed deltas and usage flow
  to the TUI over a bounded `mpsc` channel, and no lock is held across an
  `.await`.
- Streaming uses SSE over `bytes_stream()`, handles `: keep-alive`
  comments, and accumulates `input_json_delta` fragments per block.
- The screen redraws only on change, keeps wrapped lines cached per
  message (keyed by width), and pins the view to the tail with scrollback.

### 4.8 Later levers (measured before adopting)

These stay off by default until the benchmark says they pay:

- off-peak scheduling of batch or subagent work (flat 50% off);
- routing a turn to v4-pro only on escalation;
- a fixed dispatcher tool (`call(name, args)`) plus tool search, if the
  MCP tool count makes definitions large (they are only worth it past
  roughly 20k tokens);
- a subagent shared-prefix wait (run the first call, then its siblings);
- a ~1k-token tree-sitter repo map;
- search/replace edit tools.

### 4.9 Configuration surface

Everything above is a setting with a default, rendered by the HM module
the same way as the existing knobs:

- `thinking = { mode, effort, routineEffort, escalationEffort }`
- `toolOutput = { inlineMax, previewHead, previewTail, blobDir }`
- `context = { maskTrigger, maskKeep, maskClearAtLeast, compactTrigger, continueMax }`
- `anthropic = { cacheTtl, contextEditing, serverCompaction }`
- `ledger = { path, enable }`
- `prices` overrides, for when the price table drifts before a release
- `metadataUserId`, stable per user by default and documented as
  partitioning the cache

## 5. Test plan

The suite is ordered so that each gate proves something the gate before it
could not.

| Tier | Test | Proves | Negative control |
|---|---|---|---|
| Unit | `request_prefix_is_append_only`: for a scripted 20-turn conversation with tools, thinking, masking-off and blob stubs, the canonical bytes of the messages at turn *k* are a byte prefix of those at turn *k+1*. | The prefix is stable across every block kind | Inject a timestamp into the system prompt; the test must fail |
| Unit (proptest) | `tool_order_is_registration_independent`: shuffled MCP registration orders produce identical request bytes. | A10 can't regress | Replace the sort with `HashMap` iteration; it must fail |
| Unit | Thinking echo follows `EchoRule` for every (provider × tools present × thinking) combination, with one row per `EchoRule` variant enforced by an exhaustive match. | A2 is fixed for every variant, including future ones | Adding a variant without a row doesn't compile |
| Unit | Blob store: round-trip, dedup, and a stub that is byte-identical across re-runs. | A6 | — |
| Unit | `ContextPolicy` on a synthetic token trace: masks only when `clear_at_least` is met, and never masks the newest `keep` observations. | Masking is batched | Set `clear_at_least = 0`; the simulated cost must get *worse* |
| Integration | A mock provider (local axum server behind `with_base_url`) records every body: retries are byte-identical, a 503 then 200 retries, and a context-limit 400 triggers one forced mask and then succeeds. | A5, A7 | A 400 that isn't a context-limit error is not retried |
| Simulation (CI gate) | `cachesim`: replays recorded or scripted sessions through a model of DeepSeek's full-match prefix units, then computes hit ratio and cost from the price table. The gate is hit ratio ≥ 0.90 on the scripted suite. | The whole pipeline is cache-shaped | Run the same suite with the tool sort disabled; the gate must go red, which shows the metric can fail |
| Memory | A `dhat` test: bytes allocated to build turn *k*'s request grow with the *new* content, not with *k*. A criterion benchmark tracks `body()`, serialization and the render frame. | A5, A9 | The current cloning `body()` fails the growth assertion (red first) |
| Live (opt-in, needs balance) | `arnes bench`: a fixed task set against DeepSeek writes ledger JSONL, and DuckDB reports hit ratio, cost per task and output tokens per task, compared against the previous run's baseline. | Real-world effect, measured by DeepSeek's own usage fields | Run once with `metadata.user_id` randomized per request; the hit ratio should collapse |

The live tier also settles the open questions in §7. Every assumption
that is currently an inference gets a row that turns it into a measured
fact or retires it.

## 6. Phases

Each phase ends green on its tier of the suite and is shipped (committed,
nix repo bumped) before the next one starts.

| Phase | Scope | Exit criterion |
|---|---|---|
| **0 — Correctness now** | A1 (`deepseek-flash` default + catalogue check), A2 (`EchoRule::WhenTools`), A3 (`ThinkingMode`/effort), A7 guard arms, A11 (one `max_tokens` default) | Unit tier green; one live `deepseek-flash` call once funded |
| **1 — Measure** (**realized**, except the live baseline) | Usage ledger, TUI status line, `Usage::total`, price table, DuckDB queries | Ledger rows for every call; a first baseline from a live session. The baseline is blocked on the DeepSeek balance (402) |
| **2 — Stable prefix** | Borrowed request, serialize once, sorted and frozen tools, `SystemPrompt` blocks, appended environment deltas, `request_prefix_is_append_only`, `cachesim` gate | `cachesim` ≥ 0.90 with a working negative control |
| **3 — Bound context** | Blob store + `read_blob`, `ContextPolicy` masking/compaction, `MaxTokens` continuation | Policy and integration tiers green; live cost per task below the Phase 1 baseline |
| **4 — Memory and UI** | `Arc` history, channel-driven turn task, SSE streaming, render-on-change | `dhat` growth test green; no lock held across `.await` (clippy `await_holding_lock` denied) |
| **5 — Anthropic parity** | Breakpoints, TTL, optional context editing and server compaction | Mock tier asserts breakpoint placement; a live Anthropic cache read > 0 |
| **6 — Measured levers** | §4.8 items, each behind a setting and adopted only on benchmark evidence | Each adoption has a before/after ledger comparison committed |

Phase 0 is small and fixes live defects, so it goes first even though it
saves the least.

## 7. Open questions (inferences to measure, not facts)

- What the Anthropic-compatible endpoint puts in `usage`, and whether
  `input_tokens` includes cached tokens. Record raw first.
- Whether the tool-present thinking-echo requirement applies to
  Anthropic-format `thinking` blocks. Probe: a two-turn tool conversation
  with thinking stripped; expect 400.
- DeepSeek's current prefix-unit granularity, and the latency between
  writing a cache and it becoming readable (a third party measured about
  2.5 s).
- Whether `system_prompt_update: in-history` means a mid-conversation
  system message is honoured without breaking the cached prefix. Probe
  before adopting §4.2's appended-environment design on pro.

All of these are blocked on the account balance (402 as of 2026-09-26).
