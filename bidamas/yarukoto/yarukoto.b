@moduledoc """
yarukoto (やること) — things to do: the task list the model keeps, in blue.

One `TodoWrite` tool, the shape Claude Code, opencode and deepagents give
their models, so a model trained on any of them calls it unprompted. The
model sends the whole list each time; the answer is the list drawn back
as a checklist with a count, which puts the plan at the end of the context
where the model reads it best (Manus's recitation). A list that breaks a
rule is refused with the rule, so the model rewrites it.

The tool is pure: it reads only its input, keeps no store and asks for no
effect, so the kernel lets it run unasked (the `pure-tool` export kind).

    status_mark("completed") #=> "[x]"
"""

use("deeta", %i[get_str is_doc])
use("ronri", %i[count_if every])
use("yushutsu", %i[render rows])

def statuses()
  %w[pending in_progress completed]
end

def item_schema()
  {
    type: "object",
    properties: {
      content: {type: "string", minLength: 1},
      status: {type: "string", enum: statuses},
      activeForm: {type: "string"}
    },
    required: %w[content status]
  }
end

def schema()
  json_stringify(
    {
      type: "object",
      properties: {todos: {type: "array", items: item_schema}},
      required: ["todos"]
    }
  )
end

def description()
  "Keep a task list for this work: send the whole list each call, each item with content and status (pending, in_progress, completed), at most one in_progress. Use it for work of three or more steps; mark an item completed as soon as it is done."
end

@doc """
exports: What arnes hosts from this package, as `yushutsu` rows.
"""
def exports()
  render(
    [
      ["pure-tool", "TodoWrite", "write", description],
      ["schema", "TodoWrite", schema]
    ]
  )
end

@doc """
status_mark: The checkbox a status draws as, or nil for an unknown status.

    status_mark("pending") #=> "[ ]"
"""
def status_mark(s)
  if s == "completed"
    "[x]"
  elsif s == "in_progress"
    "[>]"
  elsif s == "pending"
    "[ ]"
  end
end

def todos_of(input)
  doc = json_parse(input) rescue nil
  if is_doc(doc)
    v = json_get(doc, :todos) rescue nil
    if list?(v) and not v.nil?
      v
    end
  end
end

def valid_item?(t)
  is_doc(t) and
    get_str(t, "content", "") != "" and
    not status_mark(get_str(t, "status", "")).nil?
end

def line(t)
  s = get_str(t, "status", "")
  doing = get_str(t, "activeForm", "")
  if s == "in_progress" and doing != ""
    "#{status_mark(s)} #{get_str(t, "content", "")} (#{doing})"
  else
    "#{status_mark(s)} #{get_str(t, "content", "")}"
  end
end

def done(ts)
  count_if((t) -> get_str(t, "status", "") == "completed", ts)
end

def doing(ts)
  count_if((t) -> get_str(t, "status", "") == "in_progress", ts)
end

@doc """
check: The rule a list breaks, or nil when it keeps them all.

    check(nil) #=> "TodoWrite needs todos: a list of {content, status}"
"""
def check(ts)
  if ts.nil?
    "TodoWrite needs todos: a list of {content, status}"
  elsif not every(valid_item?, ts)
    "each todo needs a non-empty content and a status of pending, in_progress or completed"
  elsif doing(ts) > 1
    "at most one todo is in_progress; finish one before you start the next"
  end
end

def summary(ts)
  n = length(ts)
  if n == 0
    "The list is empty."
  elsif done(ts) == n
    "#{n}/#{n} done. Every item is complete."
  else
    "#{done(ts)}/#{n} done. Keep the list current as you work."
  end
end

@doc """
write: The TodoWrite tool: the list drawn back as a checklist, or the rule it breaks.
"""
def write(input)
  ts = todos_of(input)
  wrong = check(ts)
  if wrong.nil?
    join(append(map(line, ts), [summary(ts)]), line_break)
  else
    "refused: #{wrong}"
  end
end

def todo(content, status)
  "{\"content\":\"#{content}\",\"status\":\"#{status}\"}"
end

def call(items)
  "{\"todos\":[#{join(items, ",")}]}"
end

test "the exports are one pure tool and its schema"
  rs = rows(exports)
  assert take(3, rs[0]) == %w[pure-tool TodoWrite write]
  assert take(2, rs[1]) == %w[schema TodoWrite]
  assert json_get(json_parse(schema), :required) == ["todos"]
  assert length(rs) == 2
end

test "a list is drawn back as a checklist with its count"
  out = write(
    call(
      [
        todo("read", "completed"),
        todo("edit", "in_progress"),
        todo("test", "pending")
      ]
    )
  )
  assert out ==
    join_lines(
      [
        "[x] read",
        "[>] edit",
        "[ ] test",
        "1/3 done. Keep the list current as you work."
      ]
    )
end

test "the active form is shown beside the item in progress"
  item = "{\"content\":\"edit\",\"status\":\"in_progress\",\"activeForm\":\"editing\"}"
  assert write(call([item])) ==
    join_lines(
      ["[>] edit (editing)", "0/1 done. Keep the list current as you work."]
    )
end

test "a finished list and an empty one"
  assert write(call([todo("a", "completed")])) ==
    join_lines(["[x] a", "1/1 done. Every item is complete."])
  assert write(call([])) == "The list is empty."
end

test "a broken list is refused with the rule it breaks"
  assert write("{}") ==
    "refused: TodoWrite needs todos: a list of {content, status}"
  assert write("not json") ==
    "refused: TodoWrite needs todos: a list of {content, status}"
  assert write(call([todo("a", "doing")])) ==
    "refused: each todo needs a non-empty content and a status of pending, in_progress or completed"
  assert write(call([todo("", "pending")])) !=
    write(call([todo("x", "pending")]))
  assert write(call([todo("a", "in_progress"), todo("b", "in_progress")])) ==
    "refused: at most one todo is in_progress; finish one before you start the next"
end
