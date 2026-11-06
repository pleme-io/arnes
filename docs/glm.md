# GLM in arnes: the best harness for it, measured

**Tier: DESIGN** (2026-11-06), except where the ledger says otherwise.

Operator, 2026-11-06: *research how to build the best harnesses, research GLM
fully, and make our harness better for GLM than GLM; make everything
extremely configurable and malleable with blue, introducing structure only
where structure should exist.*

## 1. Destination

arnes scores higher than `claude-glm` on the same GLM model, on a suite we
run ourselves, at equal or lower credits per solved task. That is the only
meaning of "better for GLM than GLM" that can be checked. Everything that is
about a model or a provider is a blue declaration. Rust keeps only what is
real structure: the Anthropic wire format, streaming, the network, keys,
tool execution and the screen.

**Structure only where it belongs.** Today each vendor fact is a Rust match
arm in `arnes-config` (`Provider::endpoint`, `default_model`, `echo_rule`,
`models_url`, `key_env`, and the ledger's peak table). These are data. They
become rows that a blue bidama declares: endpoint, models with their limits,
auth, echo rule, effort levels, sampling defaults, cache behaviour, peak
hours, credit tariff, error-code meanings, and prompt fragments. The Rust
`Provider` enum keeps only the wire dialect (`anthropic-messages` today), so
a new provider is a declaration, not a build. The kernel ratchet goes down.

### World facts

| Fact | Source | Consequence |
|---|---|---|
| The Coding Plan quota is for listed tools only. "SDK-based access or other third-party integrations" and "general-purpose API access" are prohibited, with rate limits, freezing and a ban after 3 violations | docs.z.ai `devpack/tool/others.md`, `legal-agreement/subscription-terms.md`, `devpack/usage-policy.md` (accessed 2026-11-06) | arnes on the plan key risks the account. A route with a pay-as-you-go key avoids this; the plan key is the operator's decision (§7) |
| GLM-5.3: 1M context, 128K output, text only; GLM-5.3-Flash: 1M, 128K, images and video, 3× the plan quota | `guides/llm/glm-5.3.md`, `guides/vlm/glm-5.3-flash.md` | arnes's default `max_tokens` of 8192 starves a thinking GLM. Output limits are per model facts |
| GLM always thinks. Effort is `low`, `high` or `max`, default `max`; on the Anthropic endpoint `disabled` maps to `low` | `guides/capabilities/thinking.md`, `devpack/latest-model.md`; probed: `low` produced no thinking block, `high` and `max` did | effort is the one lever. It is set per phase by policy |
| Interleaved and preserved thinking: blocks must go back complete, unmodified and in order with tool results, or quality and cache hits fall | `guides/capabilities/thinking-mode.md`; probed: an echoed block is accepted, and a tampered signature is accepted too | `EchoRule::Always` for GLM (shipped). Never trim or reorder a GLM thinking block |
| Caching is automatic and shows as `cache_read_input_tokens` on the Anthropic endpoint | probed: 6,144 of about 6,180 prompt tokens read from cache on a repeat | keep the prefix byte-stable. No `cache_control` is needed for GLM |
| Sampling defaults temperature 1.0, top_p 0.95; do not tune both | `guides/overview/migrate-to-glm-new.md` | profile defaults, not arnes globals |
| Streaming and tool-argument streaming work (`thinking_delta`, `input_json_delta`, `signature_delta`); usage arrives in `message_delta` | probed | streaming transport is required for parity with what Z.ai benchmarks against |
| Parallel tool calls and `tool_choice` work | probed: two `tool_use` blocks in one turn | the loop must run parallel read calls concurrently |
| Error codes: 1302 rate, 1305 overloaded, 1308 and 1316–1321 window limits with a reset time, 1310 weekly, 1113 no plan or unsupported tool, 1313 fair-use | `api-reference/api-code.md` | each maps to a typed verdict (`throttled`, `overloaded`, `quota-exhausted` with its reset, `refused`). Data in the GLM profile |
| Credits: (input × 6.9 + cached × 1.7 + output × 24) / 10,000 on GLM-5.3, half off-peak (06:00–10:00 UTC weekdays is peak); 28,000 per 5 h, 140,000 per week on Max | `devpack/overview.md` | the ledger reads a tariff in credits, not only USD, and a quota segment shows the windows |
| Z.ai benchmarks GLM-5.3 inside Claude Code at max effort, with 400K to 1M context and 64K to 128K output | Hugging Face `zai-org/GLM-5.3` card | that is the bar to beat, and the settings to match first |

## 2. Harness practices, ranked by measured effect

From the research report of 2026-11-06 (sources in §8).

| # | Practice | Evidence | arnes today |
|---|---|---|---|
| 1 | Edit tool shaped to the model; never line-number patches for a model not trained on them | hashline edits +15 points over 16 models; Aider udiff vs search/replace 61% vs 20% | `Edit` is exact-string replace only |
| 2 | Syntax check on every edit, reject a broken edit | SWE-agent: 18.0% with linter, 15.0% without | none |
| 3 | Bounded, summarised reads and searches | SWE-agent: 100-line window 18.0% vs full file 12.7%; summarised search 18.0% vs iterative 12.0% | `Read` defaults to 2,000 lines; `Grep` unbounded |
| 4 | Verify before stop: plan, build, test, fix; a checklist before exit | deepagents 52.8% → 66.5% Terminal-Bench 2.0, harness only | none |
| 5 | Reasoning effort per phase (max to plan and verify, high to build) | 53.9% all-xhigh vs 66.5% per phase | one effort per session (`Alt-T`) |
| 6 | Loop detection: N edits to one file, try another approach | part of #4's bundle | none |
| 7 | Persistence, tool-use and planning reminders in the system prompt | about +20% internal SWE-bench (OpenAI) | no default system prompt |
| 8 | Tool descriptions as carefully as the prompt; errors that say what to fix | vendor claim | short descriptions |
| 9 | Compaction at a threshold, tool-result clearing first | OpenHands: same score, under half the cost per turn | `ContextExceeded` only |
| 10 | Memory files at start, other files just in time | context rot measured on 18 models | `CLAUDE.md`/`AGENTS.md` loading (`docs/context-parity.md`) |
| 11 | Byte-stable prefix: no timestamps, deterministic JSON, append-only | Manus; Anthropic docs | tools sorted; partly done |
| 12 | A todo list the model rewrites | anecdote | none |
| 13 | Keep failed actions and errors in context | anecdote | kept |
| 14 | Sub-agents for read, research and review; writes on one thread | Anthropic research system +90%, at about 15× tokens | `docs/channels.md` C2 |
| 15 | Architect/editor pairs; best-of-N with a critic | Aider 77.4% → 80.5%; OpenHands 60.6% → 66.4% | channels make this a policy |

## 3. What is blue, what is Rust

| Piece | Rung | Why |
|---|---|---|
| provider profiles: endpoint, models and limits, auth, echo, effort levels, sampling, cache rule, peak hours, tariff, error codes, prompt fragments | **blue** declarations, one bidama per provider family | facts that change with a vendor's release notes |
| the system prompt, its fragments and their order per route | **blue** | the most measured prompt lever, and it varies by model |
| effort per phase, compaction trigger and summary, loop nudge, verify-before-stop checklist, todo tool | **blue** policy events (`choose-effort`, `compact`, `before-stop`, `after-edit`) and blue tools | judgment, replaceable per user |
| edit format per route; the after-edit syntax check | **blue** chooses and calls a declared checker | which format suits which model is data |
| wire format, SSE streaming, parallel tool execution, the network, keys, the gate | **Rust** | world facts: network, secrets, concurrency |
| the quota and credit segment | **blue** segment over the ledger's facts | display policy |
| Z.ai MCP servers (search, reader, zread) | **Rust** streamable-HTTP MCP transport, once; the servers are config rows | network |

## 4. Phases

Each ends green, with its tests, and raises or lowers its kernel rows in the
same commit.

| Phase | Builds | Done means |
|---|---|---|
| **G0 measure** | a GLM eval suite in-repo: 20 to 40 small coding tasks with checkers, run headless on arnes and on `claude-glm -p`, credits and pass rate to the ledger, DuckDB report | a baseline for both harnesses; the report can go red (a planted wrong checker fails it) |
| **G1 profiles as blue data** | the provider facts move out of Rust match arms into blue declarations; `max_tokens`, effort levels, sampling and the credit tariff per model | `arnes-config` Rust shrinks; GLM runs with 128K output and its own sampling; a new provider row needs no Rust |
| **G2 streaming and parallel tools** | SSE transport with `thinking_delta`, `input_json_delta`, `signature_delta`; parallel read-only tool calls run concurrently | a streamed GLM turn renders as it arrives; two reads run at once in a test |
| **G3 the default prompt and per-phase effort** | the blue system prompt (persistence, tool use, plan first, "done when"), `choose-effort` per phase | G0 suite delta measured, kept only if it moves |
| **G4 edits that cannot break the build** | the `after-edit` check, bounded reads and searches, the loop nudge, `before-stop` verify | G0 suite delta measured |
| **G5 compaction** | threshold compaction, tool-result clearing first, summary policy in blue | a long G0 task passes under a capped context |
| **G6 Z.ai tools and Flash** | streamable-HTTP MCP for the Z.ai servers; Flash for background and image work through `wariate` | a web search and a zread call in a test against a recorded server |
| **G7 quota** | credit tariff, the 5 h and weekly windows, error codes as typed verdicts, the quota segment | a 1308 with a reset time becomes `quota-exhausted` with that time on the status line |

## 5. States

A GLM turn is `streaming`, `tool-running`, `throttled` (1302, backoff),
`overloaded` (1305), `quota-exhausted` (1308, 1310, 1316–1321, with the reset
time), `refused` (1113, 1309, 1311, 1313: not retried), or `blind` (no answer).
`quota-exhausted` and `refused` are never retried by the client; `wariate`
moves the turn or the operator waits.

## 6. Ledger

| Piece | Design | Exists | Tested | Live |
|---|---|---|---|---|
| `glm` provider, catalogue, fleet key via nix | ✅ | ✅ | ✅ | ✅ |
| echo every thinking block | ✅ | ✅ | ✅ | ✅ |
| per-provider peak hours | ✅ | ✅ | ✅ | ✅ |
| G1 provider facts as blue data (`teikyou`), a provider is rows | ✅ | ✅ | ✅ | ✅ |
| G1 per-model `max_tokens`, sampling, credit tariff | ✅ | ❌ | ❌ | ❌ |
| G2 SSE streaming (thinking, text, signature, tool input deltas) | ✅ | ✅ | ✅ | ✅ on llama-server; GLM waits on credit |
| G2 parallel read-only tool calls | ✅ | ✅ | ✅ | ❌ |
| G5 compaction on overflow and `/compact` | ✅ | ✅ | ✅ | ❌ |
| G5 threshold compaction, tool-result clearing first | ✅ | ❌ | ❌ | ❌ |
| G7 spent balance or quota (1113, 1308, 1310) fails over unretried as `quota-exhausted` | ✅ | ✅ | ✅ | ❌ |
| G4 loop nudge (`turn.loop-nudge`) | ✅ | ✅ | ✅ | ❌ |
| G0, G3, the rest of G4, G6, the rest of G7 | ✅ | ❌ | ❌ | ❌ |

## 7. Open, the operator's

- **Answered 2026-11-06:** arnes uses the pay-as-you-go key (`glm/api/drzln/normal/key`, the `glm-payg` route). It serves the Anthropic endpoint: the catalogue answered 200. A message answered 429 code 1113, no balance: the account needs credit before GLM answers.
- Which key arnes uses for GLM. The plan key is outside the terms for a
  third-party harness (§1). A pay-as-you-go Z.ai key is within them, but
  whether it serves the Anthropic endpoint is not documented; the standard
  API is the OpenAI format at `/api/paas/v4`.

## 8. Sources

- Harness: SWE-agent https://arxiv.org/html/2405.15793 · hashline https://blog.can.ac/2026/02/12/the-harness-problem · Aider https://aider.chat/2023/12/21/unified-diffs.html, https://aider.chat/2024/09/26/architect.html · deepagents https://www.langchain.com/blog/improving-deep-agents-with-harness-engineering · OpenAI https://developers.openai.com/cookbook/examples/gpt4-1_prompting_guide · Anthropic https://www.anthropic.com/engineering/writing-tools-for-agents, https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents, https://www.anthropic.com/engineering/multi-agent-research-system · OpenHands https://www.openhands.dev/blog/openhands-context-condensensation-for-more-efficient-ai-agents · Manus https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus · context rot https://research.trychroma.com/context-rot
- GLM: https://docs.z.ai/guides/llm/glm-5.3.md · https://docs.z.ai/guides/capabilities/thinking.md · https://docs.z.ai/guides/capabilities/thinking-mode.md · https://docs.z.ai/devpack/latest-model.md · https://docs.z.ai/devpack/overview.md · https://docs.z.ai/api-reference/api-code.md · https://docs.z.ai/devpack/tool/others.md · https://docs.z.ai/legal-agreement/subscription-terms.md · https://huggingface.co/zai-org/GLM-5.3
- Probes (2026-11-06, ryn): effort levels, caching, echo and signatures, parallel tools, `tool_choice`, SSE events.
