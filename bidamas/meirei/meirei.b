@moduledoc """
meirei (命令) — arnes's built-in slash commands, in blue.

Each command is `meirei::<name>(args)`: it receives the text typed after
`/name` and returns a string for the status hint, or nil. What it does to
arnes goes through the host API (`arnes_*`), whose calls are effects the
kernel applies after the call succeeds. The kernel resolves `/name` to this
package by name, so a bidama called `meirei` earlier on arnes's path replaces
the whole set, and a user command called `name` replaces one.

`exports()` is what arnes reads to host this package, written as rows
through `yushutsu`. The kernel refuses a bad row alone and hosts the rest.

Host calls exist only inside arnes, so the commands are tested by
arnes-blue; the exports are tested here.
"""

use("ronri", [:every])
use("yushutsu", [:column, :render, :rows])

def exports()
  render(
    [
      ["command", "clear", "clear", "start a new conversation"],
      ["command", "exit", "exit", "quit arnes"],
      ["command", "help", "help", "show keyboard shortcuts"],
      ["command", "mcp", "mcp", "browse MCP servers, tools and schemas"],
      ["command", "mode", "mode", "permission mode: default | plan | bypass"],
      ["command", "model", "model", "switch model or provider route"],
      ["command", "notices", "notices", "review this session's notices"],
      [
        "command",
        "profile",
        "profile",
        "switch profile: MCP servers, skills, context, access"
      ],
      ["command", "resume", "resume", "switch to a recorded session"],
      [
        "command",
        "vigy",
        "vigy",
        "the vigies and their last results; /vigy tick <name>"
      ],
      ["command", "vim", "vim", "toggle vim editing"]
    ]
  )
end

test "the exports are command rows, each naming its own meirei function"
  rs = rows(exports)
  names = column(rs, 1)
  assert names ==
    [
      "clear",
      "exit",
      "help",
      "mcp",
      "mode",
      "model",
      "notices",
      "profile",
      "resume",
      "vigy",
      "vim"
    ]
  assert every((k) -> k == "command", column(rs, 0))
  assert column(rs, 2) == names
  assert names != []
end

# waive B0001: arnes host call
def clear(_args)
  arnes_clear()
  "new conversation"
end

# waive B0001: arnes host call
@doc """
exit: Send pid an exit signal: it ends with reason unless it traps exits (:normal ends nothing, :kill cannot be trapped). exit(self, r) raises.
"""
def exit(_args)
  arnes_exit()
  nil
end

# waive B0001: arnes host call
def help(_args)
  arnes_panel("help", arnes_bindings())
  nil
end

# waive B0001: arnes host call
def mode(args)
  arnes_set("permission-mode", args)
  nil
end

# waive B0001: arnes host call
def notices(_args)
  arnes_panel("notices", arnes_notices())
  nil
end

# waive B0001: arnes host call
def vim(_args)
  if arnes_state("vim") == "on"
    arnes_set("vim", "off")
    "vim editing off"
  else
    arnes_set("vim", "on")
    nil
  end
end

# waive B0001: arnes host call
def resume(args)
  if args == ""
    arnes_panel("sessions", arnes_sessions())
    "/resume <n> or /resume <id prefix>"
  else
    arnes_resume(args)
    nil
  end
end

# waive B0001: arnes host call
def model(args)
  if args == ""
    arnes_pick("model")
  else
    arnes_route_switch(args)
  end
  nil
end

# waive B0001: arnes host call
@doc """
profile: /profile opens the profile picker; /profile <name> switches (- for none).
"""
def profile(args)
  if args == ""
    arnes_pick("profile")
  else
    arnes_profile(args)
  end
  nil
end

# waive B0001: arnes host call
def mcp(args)
  arnes_open("mcp", args)
  nil
end

# waive B0001: arnes host call
def vigy(args)
  arnes_open("vigy", args)
  nil
end
