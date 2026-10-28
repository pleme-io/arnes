@moduledoc """
yushutsu (輸出) — export: how a hosted bidama tells arnes what it serves.

arnes asks each hosted bidama for `exports()`, one line per entry.
The line is fields separated by one space: `command <name> <function>
<description>`, `segment <name> <function> <description>`, `bind <action>
<chord>`. Every arnes bidama writes its exports as rows of fields through
`yushutsu::render`, and reads them back in its tests through
`yushutsu::rows`, so the line shape lives in one place.

    line([]) #=> ""
"""

use("moji", [:lines, :words])
use("retsu", [:first])

@doc """
line: One export line from its fields.

    line([]) #=> ""
"""
def line(fields)
  join(fields, " ")
end

@doc """
render: The exports text from rows of fields.
"""
def render(rows)
  join(map(line, rows), line_break)
end

@doc """
rows: The rows of an exports text, each line split into its words.
"""
def rows(text)
  map((l) -> words(l), lines(text))
end

@doc """
column: The field at `i` of every row.
"""
def column(rs, i)
  map((r) -> r[i], rs)
end

test "a line is its fields joined by one space"
  assert line(["command", "x", "x", "does", "x"]) == "command x x does x"
  assert line([]) == ""
end

test "rows round-trip through the text"
  rs = [["segment", "a", "a"], ["bind", "app:exit", "ctrl+d"]]
  assert rows(render(rs)) == rs
  assert column(rs, 1) == ["a", "app:exit"]
  assert render(rs) != render([first(rs)])
end
