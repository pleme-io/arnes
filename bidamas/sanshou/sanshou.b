@moduledoc """
sanshou (参照) — reference: `@path` mentions in a prompt, attached.

A before-submit hook: each `@path` token naming a readable file no larger
than arnes's `mention-max-bytes` is appended to the prompt as a fenced
block, once per path, and the hint says how many were attached. A path
that is missing, a directory or too large is left as typed. Files are read
through the host (`arnes_read_file`), the one filesystem capability a hosted
bidama has.

    path("x@y") #=> ""
"""

use(
  "moji",
  %i[drop_chars ends_with includes present starts_with take_chars words]
)

use("retsu", %i[contains first push])
use("yushutsu", [:render])

def exports()
  render([["hook", "before-submit", "expand", "attach @path mentions"]])
end

@doc """
path: A mention token's path: the text after `@`, trailing punctuation dropped.

    path("x@y") #=> ""
"""
def path(token)
  if starts_with(token, "@")
    trim_end(drop_chars(token, 1))
  else
    ""
  end
end

def trim_end(p)
  if present(p) and
      includes(",.;:)", take_chars(drop_chars(p, length(p) - 1), 1))
    trim_end(take_chars(p, length(p) - 1))
  else
    p
  end
end

@doc """
paths: The paths mentioned in `prompt`, in order, each once.

    paths("no mentions") #=> []
"""
def paths(prompt)
  ps = filter((p) -> present(p), map(path, words(prompt)))
  reduce(once, [], ps)
end

def once(acc, p)
  if contains(acc, p)
    acc
  else
    push(acc, p)
  end
end

def line_end(text)
  if ends_with(text, line_break)
    text
  else
    unlines([text])
  end
end

def block(p, text)
  body = line_end(text)
  join_lines(["", "", "--- @#{p} ---", "#{body}--- end @#{p} ---"])
end

# waive B0001: arnes host call
@doc """
expand: The hook: the prompt with every attachable mention appended, or nil when
there is none (the prompt goes as typed).
"""
def expand(prompt)
  max = to_int(arnes_state("mention-max-bytes"))
  found = filter(
    (pt) -> pt[1] != nil,
    map((p) -> [p, arnes_read_file(p, max)], paths(prompt))
  )
  if length(found) == 0
    nil
  else
    arnes_hint("attached #{length(found)} file(s)")
    reduce((acc, pt) -> "#{acc}#{block(first(pt), pt[1])}", prompt, found)
  end
end

test "mention paths drop trailing punctuation and repeat once"
  assert paths("see @a.rs, and @b/c.md). @a.rs again @") == %w[a.rs b/c.md]
  assert paths("no mentions") == []
  assert path("x@y") == ""
end

test "a block fences the text with its path"
  assert block("a", "x") ==
    join_lines(["", "", "--- @a ---", "x", "--- end @a ---"])
  assert block("a", unlines(["x"])) == block("a", "x")
end
