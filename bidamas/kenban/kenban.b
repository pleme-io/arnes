@moduledoc """
kenban (鍵盤) — arnes's default key bindings, in blue.

One `bind <action> <chord>` line per binding, read by the kernel as this
package's exports: the action is an arnes action id (`app:interrupt`,
`chat:submit`), the chord one or two keys (`ctrl+x ctrl+e`). They are
Claude Code's defaults. A bidama called `kenban` earlier on arnes's path
replaces the whole keymap; `keys.prompt` in the config edits it (bind,
unbind) on top, entry by entry.
"""

use("moji", [:includes])
use("retsu", [:first])
use("ronri", [:every])
use("yushutsu", %i[column render rows])

def exports()
  render(
    [
      %w[bind app:interrupt ctrl+c],
      %w[bind app:exit ctrl+d],
      %w[bind chat:cancel escape],
      %w[bind app:redraw ctrl+l],
      %w[bind app:toggleTranscript ctrl+o],
      %w[bind chat:thinkingToggle alt+t],
      %w[bind app:cycleMode shift+tab],
      %w[bind app:scrollback pageup],
      %w[bind app:scrollback shift+up],
      %w[bind chat:submit enter],
      %w[bind chat:newline shift+enter],
      %w[bind chat:newline alt+enter],
      %w[bind chat:newline ctrl+j],
      %w[bind chat:externalEditor ctrl+g],
      %w[bind chat:externalEditor ctrl+x ctrl+e],
      %w[bind chat:stash ctrl+s],
      %w[bind history:previous up],
      %w[bind history:previous ctrl+p],
      %w[bind history:next down],
      %w[bind history:next ctrl+n],
      %w[bind history:search ctrl+r],
      %w[bind edit:left left],
      %w[bind edit:left ctrl+b],
      %w[bind edit:right right],
      %w[bind edit:right ctrl+f],
      %w[bind edit:wordLeft alt+b],
      %w[bind edit:wordLeft alt+left],
      %w[bind edit:wordRight alt+f],
      %w[bind edit:wordRight alt+right],
      %w[bind edit:lineStart ctrl+a],
      %w[bind edit:lineStart home],
      %w[bind edit:lineEnd ctrl+e],
      %w[bind edit:lineEnd end],
      %w[bind edit:deleteBack backspace],
      %w[bind edit:deleteBack ctrl+h],
      %w[bind edit:deleteForward delete],
      %w[bind edit:killToEnd ctrl+k],
      %w[bind edit:killToStart ctrl+u],
      %w[bind edit:killWordBack ctrl+w],
      %w[bind edit:killWordBack alt+backspace],
      %w[bind edit:killWordForward alt+d],
      %w[bind edit:yank ctrl+y],
      %w[bind edit:yankPop alt+y],
      %w[bind edit:undo ctrl+_],
      %w[bind edit:undo ctrl+7]
    ]
  )
end

test "every binding is a bind row with an action id and a chord"
  rs = rows(exports)
  assert length(rs) == 45
  assert every((k) -> k == "bind", column(rs, 0))
  assert every((a) -> includes(a, ":"), column(rs, 1))
  assert first(rs) == %w[bind app:interrupt ctrl+c]
end
