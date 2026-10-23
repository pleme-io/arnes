-- arnes usage ledger — DuckDB queries over ~/.local/state/arnes/usage.jsonl
-- Each line is a forge-core cost Event: ts, cost_usd, cloud (provider),
-- service (model), resource_id (session), dimensions (string map).
--
--   duckdb -c ".read docs/queries/usage.sql"
--
-- SQL is the bridge to DuckDB, not the authored source: the standing
-- analyses move to a blue `bunseki` model (docs/token-efficiency.md §4.3).

CREATE OR REPLACE VIEW calls AS
SELECT
  ts::TIMESTAMP                                        AS ts,
  cloud                                                AS provider,
  service                                              AS model,
  resource_id                                          AS session,
  cost_usd,
  CAST(dimensions['input_tokens']                AS BIGINT) AS input_tokens,
  CAST(dimensions['output_tokens']               AS BIGINT) AS output_tokens,
  CAST(dimensions['cache_read_input_tokens']     AS BIGINT) AS cache_read,
  CAST(dimensions['cache_creation_input_tokens'] AS BIGINT) AS cache_write,
  dimensions['peak'] = 'true'                          AS peak,
  dimensions['priced'] = 'true'                        AS priced,
  dimensions['route']                                  AS route,
  dimensions['fallback'] = 'true'                      AS fallback,
  CAST(dimensions['latency_ms']                  AS BIGINT) AS latency_ms
FROM read_json_auto('~/.local/state/arnes/usage.jsonl', format = 'newline_delimited');

-- Per session: calls, tokens, cache-hit ratio, cost.
SELECT
  session, model,
  count(*)                                             AS calls,
  sum(input_tokens + cache_read + cache_write)         AS prompt_tokens,
  sum(output_tokens)                                   AS output_tokens,
  round(sum(cache_read) / nullif(sum(input_tokens + cache_read + cache_write), 0), 3) AS hit_ratio,
  round(sum(cost_usd), 4)                              AS cost_usd,
  bool_and(priced)                                     AS fully_priced
FROM calls
GROUP BY ALL
ORDER BY min(ts) DESC;

-- Per route: who answered, at what cost and latency, and how often as a fallback.
SELECT
  route, provider,
  count(*)                                             AS calls,
  round(sum(cost_usd), 4)                              AS cost_usd,
  quantile_cont(latency_ms, 0.5)                       AS p50_ms,
  quantile_cont(latency_ms, 0.95)                      AS p95_ms,
  round(avg(fallback::INT), 3)                         AS fallback_share
FROM calls
WHERE route IS NOT NULL
GROUP BY ALL
ORDER BY calls DESC;

-- Daily spend and the off-peak share.
SELECT
  date_trunc('day', ts)                                AS day,
  round(sum(cost_usd), 4)                              AS cost_usd,
  round(avg(CASE WHEN peak THEN 0 ELSE 1 END), 2)      AS offpeak_share
FROM calls
GROUP BY ALL
ORDER BY day DESC;
