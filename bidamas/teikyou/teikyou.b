@moduledoc """
teikyou (提供) — provision: the model providers arnes can reach, as data.

One `provider <name> <fact> <value>` row per fact, read by the kernel as
this package's exports. The kernel keeps only the wire: a provider is the
facts below, and a new one is rows, not a build. A `teikyou` earlier on
arnes's bidama path replaces this table; another hosted bidama may add
providers with its own `provider` rows. A fact declared twice keeps the
first, and a bad fact is refused alone.

Facts: `endpoint` (base URL; none means a route names its own), `model`
(the default model; none means a route names its own), `key-env`, `key`
(`required` or `optional`), `auth` (`x-api-key` or `bearer`), `echo` (when
prior thinking goes back: `never`, `always`, `when-tools`), `catalogue`
(`<url> <auth>`), `thinking-budget` (`honoured` or `ignored`), `peak`
(weekday UTC hours at full price, `<from>-<to>` each).

    length(provider("x", [["model", "m"]])) #=> 1
"""

use("retsu", [:first])
use("ronri", [:every])
use("yushutsu", %i[column render rows])

def provider(name, facts)
  map((f) -> append(["provider", name], f), facts)
end

def table()
  append(
    provider(
      "deepseek",
      [
        %w[endpoint https://api.deepseek.com/anthropic],
        %w[model deepseek-flash],
        %w[key-env DEEPSEEK_API_KEY],
        %w[echo when-tools],
        %w[catalogue https://api.deepseek.com/models bearer],
        %w[peak 1-4 6-10]
      ]
    ),
    provider(
      "anthropic",
      [
        %w[endpoint https://api.anthropic.com],
        %w[model claude-opus-5-5],
        %w[key-env ANTHROPIC_API_KEY],
        %w[echo always],
        %w[catalogue https://api.anthropic.com/v1/models x-api-key],
        %w[thinking-budget honoured]
      ]
    ),
    provider(
      "glm",
      [
        %w[endpoint https://api.z.ai/api/anthropic],
        %w[model glm-5.3],
        %w[key-env ZAI_API_KEY],
        %w[echo always],
        %w[catalogue https://api.z.ai/api/anthropic/v1/models x-api-key],
        %w[peak 6-10]
      ]
    ),
    provider(
      "local",
      [
        %w[endpoint http://127.0.0.1:8080],
        %w[model local],
        %w[key-env LOCAL_LLM_API_KEY],
        %w[key optional]
      ]
    ),
    provider("messages", [%w[key-env MESSAGES_API_KEY], %w[auth bearer]])
  )
end

@doc """
exports: What arnes hosts from this package, as `yushutsu` rows.
"""
def exports()
  render(table)
end

test "every row is a provider fact with a value"
  rs = rows(exports)
  assert every((k) -> k == "provider", column(rs, 0))
  assert every((r) -> length(r) >= 4, rs)
end

test "the five providers arnes ships, each named once per fact"
  names = column(rows(exports), 1)
  assert distinct(names) == %w[deepseek anthropic glm local messages]
  assert length(rows(exports)) == 24
end

test "a provider without an endpoint is one a route names"
  rs = filter((r) -> r[1] == "messages", rows(exports))
  assert every((r) -> r[2] != "endpoint", rs)
  assert first(rs) == %w[provider messages key-env MESSAGES_API_KEY]
end
