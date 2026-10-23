use("kenkou")

# MCP health, one line per tick, as a user-declared vigy. The logic is the
# kenkou bidama's (arnes hosts it with `:bidamas ("kenkou")`); this file shows
# a declared vigy reusing it:
#
#   :vigies ((:name "mcp-health" :blue "~/.config/arnes/mcp-health.b" :fn "tick" :every-ms 5000))

def tick(facts)
  kenkou::tick(facts)
end
