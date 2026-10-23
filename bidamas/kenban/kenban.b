use("moji", [:includes])
use("retsu", [:first])
use("ronri", [:every])
use("yushutsu", [:column, :render, :rows])

# kenban (鍵盤) — arnes's default key bindings, in blue.
#
# One `bind <action> <chord>` line per binding, read by the kernel as this
# package's exports: the action is an arnes action id (`app:interrupt`,
# `chat:submit`), the chord one or two keys (`ctrl+x ctrl+e`). They are
# Claude Code's defaults. A bidama called `kenban` earlier on arnes's path
# replaces the whole keymap; `keys.prompt` in the config edits it (bind,
# unbind) on top, entry by entry.

def exports()
  render(
    [
      ["bind", "app:interrupt", "ctrl+c"],
      ["bind", "app:exit", "ctrl+d"],
      ["bind", "chat:cancel", "escape"],
      ["bind", "app:redraw", "ctrl+l"],
      ["bind", "app:toggleTranscript", "ctrl+o"],
      ["bind", "chat:thinkingToggle", "alt+t"],
      ["bind", "app:cycleMode", "shift+tab"],
      ["bind", "app:scrollback", "pageup"],
      ["bind", "app:scrollback", "shift+up"],
      ["bind", "chat:submit", "enter"],
      ["bind", "chat:newline", "shift+enter"],
      ["bind", "chat:newline", "alt+enter"],
      ["bind", "chat:newline", "ctrl+j"],
      ["bind", "chat:externalEditor", "ctrl+g"],
      ["bind", "chat:externalEditor", "ctrl+x", "ctrl+e"],
      ["bind", "chat:stash", "ctrl+s"],
      ["bind", "history:previous", "up"],
      ["bind", "history:previous", "ctrl+p"],
      ["bind", "history:next", "down"],
      ["bind", "history:next", "ctrl+n"],
      ["bind", "history:search", "ctrl+r"],
      ["bind", "edit:left", "left"],
      ["bind", "edit:left", "ctrl+b"],
      ["bind", "edit:right", "right"],
      ["bind", "edit:right", "ctrl+f"],
      ["bind", "edit:wordLeft", "alt+b"],
      ["bind", "edit:wordLeft", "alt+left"],
      ["bind", "edit:wordRight", "alt+f"],
      ["bind", "edit:wordRight", "alt+right"],
      ["bind", "edit:lineStart", "ctrl+a"],
      ["bind", "edit:lineStart", "home"],
      ["bind", "edit:lineEnd", "ctrl+e"],
      ["bind", "edit:lineEnd", "end"],
      ["bind", "edit:deleteBack", "backspace"],
      ["bind", "edit:deleteBack", "ctrl+h"],
      ["bind", "edit:deleteForward", "delete"],
      ["bind", "edit:killToEnd", "ctrl+k"],
      ["bind", "edit:killToStart", "ctrl+u"],
      ["bind", "edit:killWordBack", "ctrl+w"],
      ["bind", "edit:killWordBack", "alt+backspace"],
      ["bind", "edit:killWordForward", "alt+d"],
      ["bind", "edit:yank", "ctrl+y"],
      ["bind", "edit:yankPop", "alt+y"],
      ["bind", "edit:undo", "ctrl+_"],
      ["bind", "edit:undo", "ctrl+7"]
    ]
  )
end

test "every binding is a bind row with an action id and a chord"
  rs = rows(exports)
  assert length(rs) == 45
  assert every((k) -> k == "bind", column(rs, 0))
  assert every((a) -> includes(a, ":"), column(rs, 1))
  assert first(rs) == ["bind", "app:interrupt", "ctrl+c"]
end
