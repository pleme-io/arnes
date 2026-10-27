@moduledoc """
kenkou (健康) — health: arnes's MCP health vigy, in blue.

arnes's health scheduler probes each MCP server on an adaptive schedule
(docs/profiles.md) and passes the newest facts as a JSON array of
{server, up, latency_ms, tools, error}; `kenkou::tick` turns them into the
tick's one-line result, stored in the shared vigy store and shown by /vigy.
A tick only reads the facts, so kenkou is hosted by default.

    segment("[]") #=> ""
"""

use("deeta", [:get_int, :get_str])
use("retsu", [:first])
use("yushutsu", [:render, :rows])

def exports()
  render(
    [
      ["vigy", "mcp-health", "tick", "30000"],
      ["segment", "mcp", "segment", "how many MCP servers are up"]
    ]
  )
end

def line(p)
  server = get_str(p, "server", "?")
  if json_get(p, "up")
    "#{server} #{get_int(p, "latency_ms", 0)}ms"
  else
    "#{server} down"
  end
end

@doc """
segment: The status segment: `mcp <up>/<all>`, nothing before the first probe.

    segment("[]") #=> ""
"""
def segment(facts)
  ps = json_parse(facts)
  if length(ps) == 0
    ""
  else
    "mcp #{length(filter((p) -> json_get(p, "up"), ps))}/#{length(ps)}"
  end
end

def tick(facts)
  ps = json_parse(facts)
  up = filter((p) -> json_get(p, "up"), ps)
  if length(ps) == 0
    "no MCP servers"
  else
    "#{length(up)}/#{length(ps)} up: #{join(map(line, ps), ", ")}"
  end
end

test "no servers, and a mixed pair"
  assert tick("[]") == "no MCP servers"
  two = "[{\"server\":\"a\",\"up\":true,\"latency_ms\":12},{\"server\":\"b\",\"up\":false}]"
  assert tick(two) == "1/2 up: a 12ms, b down"
  assert tick(two) != "2/2 up: a 12ms, b down"
end

test "the export is one vigy row"
  assert first(rows(exports)) == ["vigy", "mcp-health", "tick", "30000"]
  assert segment("[]") == ""
  assert segment("[{\"up\":true},{\"up\":false}]") == "mcp 1/2"
end
