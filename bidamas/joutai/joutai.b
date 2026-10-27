@moduledoc """
joutai (状態) — the status line's segments, in blue.

A segment is text the kernel lays out in the status row; the kernel owns
where it goes and how it is acknowledged, this package owns what it says.
Every segment after the first is led by the same separator, so the shape is
one function, `joutai::segment`, and each segment is one line over it.

    profile("") #=> ""
"""

use("ronri", [:every])
use("yushutsu", [:column, :render, :rows])

@doc """
exports: What arnes hosts from this package, as `yushutsu` rows.
"""
def exports()
  render(
    [
      ["segment", "model", "model", "the model in use"],
      ["segment", "effort", "effort", "reasoning effort"],
      ["segment", "tools", "tools", "how many tools this session can call"],
      ["segment", "permission-mode", "permission", "the permission mode"],
      ["segment", "idle", "idle", "the activity line when nothing runs"],
      ["segment", "profile", "profile", "the active profile"]
    ]
  )
end

def segment(text)
  " · #{text}"
end

def model(m)
  " #{m}"
end

def effort(e)
  segment("effort #{e}")
end

def permission(mode)
  segment("#{mode} mode")
end

@doc """
idle: The activity line at rest: ready, and the route it will use.

    idle("local llama.cpp · free") #=> "ready · local llama.cpp · free"
"""
def idle(route)
  "ready · #{route}"
end

@doc """
profile: Nothing when no profile is active.

    profile("") #=> ""
"""
def profile(name)
  if name == ""
    ""
  else
    segment("profile #{name}")
  end
end

def tools(count)
  segment("#{count} tools")
end

test "a segment is its text behind the separator"
  assert segment("x") == " · x"
  assert segment("") == " · "
end

test "the profile segment names it, and is empty without one"
  assert profile("") == ""
  assert profile("ops") == " · profile ops"
  assert profile("a") != " · a"
end

test "the tools segment counts"
  assert tools("0") == " · 0 tools"
  assert tools("42") == " · 42 tools"
  assert tools("1") != " · 1 tool"
end

test "the exports are segment rows, each naming a joutai function"
  rs = rows(exports)
  assert column(rs, 1) ==
    ["model", "effort", "tools", "permission-mode", "idle", "profile"]
  assert column(rs, 2) ==
    ["model", "effort", "tools", "permission", "idle", "profile"]
  assert every((k) -> k == "segment", column(rs, 0))
end

test "each segment says what the kernel said"
  assert model("m") == " m"
  assert effort("high") == " · effort high"
  assert permission("plan") == " · plan mode"
  assert permission("plan") != " · plan"
end

test "the idle line names the route"
  assert idle("local llama.cpp · free") == "ready · local llama.cpp · free"
  assert idle("") != "ready"
end
