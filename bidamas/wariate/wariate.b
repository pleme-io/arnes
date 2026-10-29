@moduledoc """
wariate (割当) — assignment: the per-turn model-route policy, which route answers the next turn and why.

    rung_index(1, 0.5, 3) #=> 2
"""

use(
  "chokkan",
  %i[
    ask
    choice
    chosen
    confidence
    expected_level
    keep_tail
    noul
    probabilities
    probability
    read
    request
    score
    score_of
  ]
)

use("deeta", %i[as_json get_int get_number get_str is_doc])
use("kazu", %i[abs clamp max])

use(
  "moji",
  %i[ascii_digits ascii_letters drop_chars is_blank made_of trim words]
)

use("retsu", %i[first is_empty last size])
use("yushutsu", %i[render rows])

def exports()
  render(
    [
      ["hook", "choose-route", "choose", "assign this turn a model route"],
      ["hook", "before-submit", "directive", "@label forces a route"],
      ["command", "route", "route", "route policy: auto | pin | off | why"]
    ]
  )
end

def put_all(m, pairs)
  reduce((acc, kv) -> assoc(acc, first(kv), last(kv)), m, pairs)
end

def named(s)
  if string?(s) and not is_blank(s)
    s
  end
end

def is_pair(p)
  list?(p) and size(p) == 2 and string?(first(p))
end

def is_object(v)
  map?(v) or is_doc(v) and every?(is_pair, v)
end

def object_of(text)
  if string?(text)
    v = json_parse(text) rescue nil
    if is_object(v)
      v
    end
  end
end

def fields_of(doc)
  if is_doc(doc)
    map((p) -> first(p), filter(is_pair, doc))
  else
    []
  end
end

def facts_of(text)
  doc = object_of(text)
  if doc.nil?
    nil
  else
    reading(doc)
  end
end

def reading(doc)
  prompt = get_str(doc, "prompt", "")
  {
    v: get_int(doc, "v", 1),
    prompt:,
    tail: get_str(doc, "tail", ""),
    turn: get_int(doc, "turn", 0),
    last_turn_tool_calls: amount(doc, "last_turn_tool_calls", 0),
    context: context_of(as_json(doc, "context"), length(prompt)),
    cache_hit_ratio: get_number(doc, "cache_hit_ratio", 0),
    current: named(get_str(doc, "current", "")),
    current_source: get_str(doc, "current_source", "auto"),
    configured_primary: named(get_str(doc, "configured_primary", "")),
    last_served: served_of(as_json(doc, "last_served")),
    routes: routes_of(as_json(doc, "routes")),
    knobs: knobs_of(as_json(doc, "knobs")),
    memory: memory_of(as_json(doc, "memory")),
    now_ms: get_number(doc, "now_ms", 0)
  }
end

def amount(doc, key, default)
  max(0, get_number(doc, key, default))
end

def context_of(c, chars)
  {
    last_prompt_tokens: amount(c, "last_prompt_tokens", 0),
    last_output_tokens: amount(c, "last_output_tokens", 0),
    prompt_chars: amount(c, "prompt_chars", chars),
    max_tokens: amount(c, "max_tokens", 0)
  }
end

def served_of(s)
  {
    label: named(get_str(s, "label", "")),
    fallback: as_json(s, "fallback") == true,
    latency_ms: get_number(s, "latency_ms", nil)
  }
end

def knobs_of(k)
  {
    judge: named(get_str(k, "judge", "")),
    min_confidence: get_number(k, "min_confidence", 70),
    stick_turns: get_number(k, "stick_turns", 3),
    cooldown_secs: get_number(k, "cooldown_secs", 300),
    allow_paid: as_json(k, "allow_paid") == true
  }
end

def routes_of(v)
  if is_doc(v)
    reduce((acc, r) -> admit(acc, route_of(r)), [], v)
  else
    []
  end
end

def admit(acc, r)
  if r.nil? or member?(r[:label], labels(acc))
    acc
  else
    append(acc, [r])
  end
end

def route_of(r)
  label = named(get_str(r, "label", ""))
  if label.nil?
    nil
  else
    health = as_json(r, "health")
    {
      label:,
      provider: get_str(r, "provider", ""),
      model: get_str(r, "model", ""),
      tools: as_json(r, "tools") == true,
      paid: as_json(r, "paid") != false,
      tier: tier_of(as_json(r, "tier")),
      describe: get_str(r, "describe", ""),
      price_out: get_number(as_json(r, "price"), "out", nil),
      context_window: window_of(as_json(r, "context_window")),
      echo: get_str(r, "echo", "never"),
      usable: as_json(r, "usable") == true,
      switch_drops_thinking: as_json(r, "switch_drops_thinking") == true,
      verdict: get_str(health, "verdict", "unknown"),
      age_ms: get_number(health, "age_ms", 0)
    }
  end
end

def tier_of(v)
  if integer?(v) and v >= 0 and v <= 9
    v
  end
end

def window_of(v)
  if number?(v) and v > 0
    v
  end
end

def memory_keys()
  %i[
    mode
    forced
    last_choice
    since_switch
    adopt
    decisions
    switches
    abstains
    last_why
  ]
end

def memory_of(m)
  {
    mode: mode_of(get_str(m, "mode", "auto")),
    forced: named(get_str(m, "forced", "")),
    last_choice: named(get_str(m, "last_choice", "")),
    since_switch: get_int(m, "since_switch", nil),
    adopt: as_json(m, "adopt") == true,
    decisions: get_int(m, "decisions", 0),
    switches: get_int(m, "switches", 0),
    abstains: counts_of(as_json(m, "abstains")),
    last_why: get_str(m, "last_why", "")
  }
end

def mode_of(s)
  if member?(s, %w[auto pin off])
    s
  else
    "auto"
  end
end

def counts_of(v)
  if is_doc(v)
    reduce(count_pair, {}, v)
  else
    {}
  end
end

def count_pair(acc, p)
  if is_pair(p) and integer?(last(p))
    assoc(acc, first(p), last(p))
  else
    acc
  end
end

def overlay(memory, doc)
  top = memory_of(doc)
  present = fields_of(doc)
  reduce((acc, k) -> take_field(acc, k, top, present), memory, memory_keys)
end

def take_field(acc, k, top, present)
  if member?(to_s(k), present)
    assoc(acc, k, get(top, k))
  else
    acc
  end
end

def recall(stored, memory)
  overlay(memory, object_of(stored))
end

def complete(memory)
  if map?(memory)
    reduce(
      (acc, k) -> keep_set(acc, k, get(memory, k)),
      memory_of(nil),
      memory_keys
    )
  else
    memory_of(nil)
  end
end

def keep_set(acc, k, v)
  if v.nil?
    acc
  else
    assoc(acc, k, v)
  end
end

def labels(routes)
  map((r) -> r[:label], routes)
end

def route_named(routes, wanted)
  find(routes, (r) -> r[:label] == wanted)
end

def answered(judged)
  map?(judged) and judged[:kind] == :answered
end

def tools_need(judged)
  if answered(judged)
    probability(judged, "tools")
  end
end

def noul_of(judged, question)
  p = probability(judged, question)
  if p.nil?
    0
  else
    p
  end
end

def level_confidence(judged)
  c = confidence(judged, "difficulty")
  if c.nil?
    0
  else
    c
  end
end

def difficulty(judged)
  ps = probabilities(judged, "difficulty")
  if is_doc(ps)
    1 + expected_level(ps)
  else
    s = score_of(judged, "difficulty")
    if s.nil?
      nil
    else
      1 + s
    end
  end
end

def cooling_verdicts()
  %w[throttled server-error unreachable timed-out]
end

def cooling(knobs, r)
  member?(r[:verdict], cooling_verdicts) and
    r[:age_ms] < knobs[:cooldown_secs] * 1000
end

def lacks_tools(need, r)
  not r[:tools] and not (need != nil and need < 0.2)
end

def demand(facts)
  c = facts[:context]
  c[:last_prompt_tokens] +
    c[:last_output_tokens] +
    c[:prompt_chars] / 3 +
    c[:max_tokens]
end

def fits(facts, r)
  w = r[:context_window]
  w.nil? or demand(facts) <= 0.9 * w
end

def paid_out(facts, r)
  r[:paid] and
    not facts[:knobs][:allow_paid] and
    r[:label] != facts[:configured_primary]
end

def drops_thinking(facts, r)
  r[:switch_drops_thinking] and
    r[:echo] == "when-tools" and
    facts[:last_turn_tool_calls] > 0 and
    r[:label] != facts[:current]
end

def objections(facts, need, r)
  [
    [not r[:usable], "not usable"],
    [r[:verdict] == "unauthorized", "unauthorized"],
    [cooling(facts[:knobs], r), "cooling down after #{r[:verdict]}"],
    [lacks_tools(need, r), "no tools"],
    [not fits(facts, r), "the context does not fit"],
    [paid_out(facts, r), "paid"],
    [drops_thinking(facts, r), "a switch would drop thinking mid-tool-run"]
  ]
end

def objection(facts, need, r)
  hit = filter((o) -> first(o), objections(facts, need, r))
  if is_empty(hit)
    nil
  else
    last(first(hit))
  end
end

def eligible_with(facts, need)
  filter((r) -> objection(facts, need, r).nil?, facts[:routes])
end

def eligible(facts, judged)
  eligible_with(facts, tools_need(judged))
end

def askable(facts)
  eligible_with(facts, 0)
end

def tier(r)
  t = r[:tier]
  if t.nil?
    -1
  else
    t
  end
end

def cost(r)
  if not r[:paid]
    -1.0
  else
    price = r[:price_out]
    if price.nil?
      1000000000.0
    else
      to_float(price)
    end
  end
end

def strength(routes)
  sort_keyed(tier, sort_keyed(cost, routes))
end

def strongest(ranked)
  top = tier(last(ranked))
  first(filter((r) -> tier(r) == top, ranked))
end

def nearest_up(ranked, least)
  up = filter((r) -> tier(r) >= least, ranked)
  if is_empty(up)
    strongest(ranked)
  else
    first(up)
  end
end

def rung_index(d, s, n)
  if s >= 0.5
    n - 1
  else
    clamp(round((d - 1) / 4 * (n - 1)), 0, n - 1)
  end
end

def one_place(d)
  round(d * 10) / 10
end

def bump(n)
  if n.nil?
    nil
  else
    n + 1
  end
end

def tally(counts, key)
  n = get(counts, key)
  if n.nil?
    1
  else
    n + 1
  end
end

def moved(current, wanted)
  if current == wanted
    0
  else
    1
  end
end

def abstain(m, key, why)
  counts = m[:abstains]
  {
    route: nil,
    why:,
    memory: put_all(
      m,
      [
        [:since_switch, bump(m[:since_switch])],
        [:abstains, assoc(counts, key, tally(counts, key))],
        [:last_why, why]
      ]
    )
  }
end

def move_to(m, current, wanted, why)
  {
    route: wanted,
    why:,
    memory: put_all(
      m,
      [
        [:since_switch, 0],
        [:last_choice, wanted],
        [:switches, m[:switches] + moved(current, wanted)],
        [:last_why, why]
      ]
    )
  }
end

def decide(facts, judged, memory)
  base = complete(memory)
  m = assoc(base, :decisions, base[:decisions] + 1)
  assoc(
    steer(facts, judged, m, eligible(facts, judged)),
    :judged,
    summary(judged)
  )
end

def steer(facts, judged, m, e)
  wanted = m[:forced]
  if wanted.nil?
    by_mode(facts, judged, m, e)
  else
    forced(facts, judged, assoc(m, :forced, nil), e, wanted)
  end
end

def forced(facts, judged, m, e, wanted)
  r = route_named(facts[:routes], wanted)
  if r.nil?
    abstain(m, "forced", "@#{wanted} is not a configured route")
  elsif member?(wanted, labels(e))
    move_to(m, facts[:current], wanted, "you asked for #{wanted}")
  else
    abstain(
      m,
      "forced",
      "@#{wanted} is not eligible: #{objection(facts, tools_need(judged), r)}"
    )
  end
end

def by_mode(facts, judged, m, e)
  mode = m[:mode]
  if mode == "off"
    abstain(m, "off", "the route policy is off")
  elsif mode == "pin"
    abstain(m, "pin", "the route is pinned")
  else
    by_source(facts, judged, m, e)
  end
end

def by_source(facts, judged, m, e)
  current = facts[:current]
  if m[:adopt]
    settle(
      facts,
      judged,
      put_all(m, [[:adopt, false], [:last_choice, current]]),
      e
    )
  elsif facts[:current_source] == "manual" and current != m[:last_choice]
    abstain(assoc(m, :mode, "pin"), "manual", "you chose the route")
  else
    settle(facts, judged, m, e)
  end
end

def settle(facts, judged, m, e)
  if is_empty(e)
    abstain(m, "empty", "no route is eligible: stay")
  elsif not member?(facts[:current], labels(e))
    lift(facts, judged, m, e)
  elsif answered(judged) and difficulty(judged) != nil
    weigh(facts, judged, m, e)
  else
    abstain(m, "no_judgment", "no judgment: stay")
  end
end

def ineligibility(facts, judged, here)
  if here.nil?
    "not a configured route"
  else
    objection(facts, tools_need(judged), here)
  end
end

def lift(facts, judged, m, e)
  current = facts[:current]
  here = route_named(facts[:routes], current)
  least = if here.nil?
    -1
  else
    tier(here)
  end
  target = nearest_up(strength(e), least)[:label]
  move_to(
    m,
    current,
    target,
    "the floor moves off an ineligible route: #{current} (#{ineligibility(facts, judged, here)}) to #{target}"
  )
end

def weigh(facts, judged, m, e)
  d = difficulty(judged)
  ranked = strength(e)
  ts = distinct(map(tier, ranked))
  at = rung_index(d, noul_of(judged, "sensitive"), size(ts))
  cand = first(filter((r) -> tier(r) == ts[at], ranked))
  pick = judges_pick(facts, judged, ranked, ts, at, cand)
  wanted = pick[:label]
  why = if wanted == cand[:label]
    "difficulty #{one_place(d)} of 5: #{wanted}"
  else
    "the judge picked #{wanted} (difficulty #{one_place(d)} of 5)"
  end
  gate(facts, judged, m, e, pick, why)
end

def judges_pick(facts, judged, ranked, ts, at, cand)
  r = route_named(ranked, chosen(judged, "route"))
  c = confidence(judged, "route")
  if r != nil and
      c != nil and
      c * 100 >= facts[:knobs][:min_confidence] and
      abs(position(tier(r), ts) - at) <= 1
    r
  else
    cand
  end
end

def sticky(m, knobs)
  n = m[:since_switch]
  n != nil and n < knobs[:stick_turns]
end

def hard_upgrade(judged, knobs, up)
  up and
    difficulty(judged) >= 4 and
    level_confidence(judged) * 100 >= knobs[:min_confidence] + 15
end

def gate(facts, judged, m, e, pick, why)
  current = facts[:current]
  knobs = facts[:knobs]
  d = difficulty(judged)
  c = noul_of(judged, "continues")
  s = noul_of(judged, "sensitive")
  here = tier(route_named(e, current))
  up = tier(pick) > here
  wanted = pick[:label]
  if tier(pick) < here and (c >= 0.5 or s >= 0.5)
    abstain(m, "no_downgrade", "no downgrade mid-task")
  elsif up and c >= 0.5 and d < 3.5
    abstain(m, "continuation", "continuation, cache kept")
  elsif sticky(m, knobs) and not hard_upgrade(judged, knobs, up)
    abstain(
      m,
      "sticky",
      "sticky: #{m[:since_switch]} of #{knobs[:stick_turns]} turns since the last switch"
    )
  elsif wanted == current
    abstain(m, "stay", "#{current} already fits (difficulty #{one_place(d)})")
  else
    move_to(m, current, wanted, why)
  end
end

def summary(judged)
  if map?(judged)
    if answered(judged)
      {
        difficulty: difficulty(judged),
        continues: probability(judged, "continues"),
        sensitive: probability(judged, "sensitive"),
        tools: probability(judged, "tools"),
        route: chosen(judged, "route"),
        route_confidence: confidence(judged, "route"),
        difficulty_confidence: confidence(judged, "difficulty")
      }
    else
      {kind: to_s(judged[:kind]), why: judged[:why]}
    end
  end
end

def difficulty_levels()
  [
    "a fact, greeting or one-line answer",
    "a small local change or short explanation",
    "a multi-step change in one area",
    "a cross-cutting change, debugging an unknown cause, or a design decision",
    "a long autonomous task where subtle mistakes are costly"
  ]
end

def description(r)
  if is_blank(r[:describe])
    r[:label]
  else
    r[:describe]
  end
end

def options(routes)
  map((r) -> [r[:label], description(r)], routes)
end

def questions(_facts, routes)
  if size(routes) < 2
    nil
  else
    [
      ask(
        "route",
        choice(
          "Which of these models is the best fit to answer the newest message?",
          options(routes)
        )
      ),
      ask(
        "continues",
        noul("Is the newest message a continuation of the task already under way (a follow-up, correction, approval or next step) rather than a new, unrelated task?")
      ),
      ask(
        "tools",
        noul("Will a good answer need to read or change files, run commands, or call external tools?")
      ),
      ask(
        "sensitive",
        noul("Does the newest message concern production systems, credentials or secrets, or an operation that cannot be undone?")
      ),
      ask(
        "difficulty",
        score("How demanding is the newest message?", difficulty_levels)
      )
    ]
  end
end

def tail_chars()
  4000
end

def state_of(facts)
  join_lines(
    [
      "Earlier conversation (oldest first, truncated):",
      keep_tail(facts[:tail], tail_chars),
      "",
      "Newest message:",
      facts[:prompt]
    ]
  )
end

def judge_request(facts)
  qs = questions(facts, askable(facts))
  if facts[:knobs][:judge].nil? or qs.nil?
    nil
  else
    request("judge-latest", state_of(facts), qs) rescue nil
  end
end

def judgment(reply)
  if reply.nil?
    nil
  else
    doc = object_of(reply)
    read(get_int(doc, "status", 0), get_str(doc, "body", nil))
  end
end

def answer(decision)
  json_stringify(
    {
      route: decision[:route],
      why: decision[:why],
      judged: decision[:judged],
      memory: decision[:memory]
    }
  )
end

def label_chars()
  concat(ascii_letters, ascii_digits, "-_")
end

def known_label(label, known)
  if known.nil?
    made_of(label, label_chars)
  else
    member?(label, known)
  end
end

def known_labels(text)
  if string?(text)
    words(text)
  end
end

def lstrip(s)
  if starts_with?(s, " ")
    lstrip(drop_chars(s, 1))
  else
    s
  end
end

def directive_of(prompt, known)
  if string?(prompt) and starts_with?(prompt, "@") and contains?(prompt, " ")
    head = first(split(prompt, " "))
    label = drop_chars(head, 1)
    if known_label(label, known)
      [label, lstrip(drop_chars(prompt, length(head) + 1))]
    end
  end
end

def forcing(stored, wanted)
  json_stringify(assoc(overlay({}, object_of(stored)), :forced, wanted))
end

def reasons()
  %w[
    off
    pin
    forced
    manual
    empty
    no_judgment
    no_downgrade
    continuation
    sticky
    stay
  ]
end

def abstain_line(counts)
  hits = filter((k) -> get(counts, k) != nil, reasons)
  if is_empty(hits)
    "none"
  else
    join(map((k) -> "#{k} #{get(counts, k)}", hits), ", ")
  end
end

def explain(memory)
  last_why = if is_blank(memory[:last_why])
    "no decision yet"
  else
    memory[:last_why]
  end
  join(
    [
      "mode: #{memory[:mode]}",
      "last: #{last_why}",
      "decisions: #{memory[:decisions]}, switches: #{memory[:switches]}",
      "abstained: #{abstain_line(memory[:abstains])}"
    ],
    line_break
  )
end

def mode_hint(word)
  if word == "auto"
    "route policy auto: each turn's route is chosen for it"
  elsif word == "pin"
    "route pinned: the current route stays"
  else
    "route policy off: the route changes only when you change it"
  end
end

def command_of(args, stored)
  word = trim(to_s(args))
  doc = object_of(stored)
  if member?(word, %w[auto pin off])
    {
      write: json_stringify(
        put_all(overlay({}, doc), [[:mode, word], [:adopt, word == "auto"]])
      ),
      panel: nil,
      hint: mode_hint(word)
    }
  elsif word == "why"
    {write: nil, panel: explain(overlay(memory_of(nil), doc)), hint: nil}
  else
    {
      write: nil,
      panel: nil,
      hint: "route policy #{overlay(memory_of(nil), doc)[:mode]}: /route auto | pin | off | why"
    }
  end
end

# waive B0001: arnes host call
def choose(facts_json)
  facts = facts_of(facts_json)
  if facts.nil?
    nil
  else
    memory = recall(arnes_store_get("memory"), facts[:memory])
    asked = judge_request(facts)
    reply = if asked.nil?
      nil
    else
      arnes_http(facts[:knobs][:judge], asked) rescue nil
    end
    d = decide(facts, judgment(reply), memory)
    arnes_store_set("memory", json_stringify(d[:memory]))
    arnes_store_set("labels", join(labels(facts[:routes]), " "))
    answer(d)
  end
end

# waive B0001: arnes host call
def directive(prompt)
  found = directive_of(prompt, known_labels(arnes_store_get("labels")))
  if found.nil?
    nil
  else
    arnes_store_set("memory", forcing(arnes_store_get("memory"), first(found)))
    last(found)
  end
end

# waive B0001: arnes host call
def route(args)
  c = command_of(args, arnes_store_get("memory"))
  if c[:write] != nil
    arnes_store_set("memory", c[:write])
  end
  if c[:panel] != nil
    arnes_panel("route", c[:panel])
  end
  c[:hint]
end

def example_route(label, rank, window)
  {
    label:,
    provider: "local",
    model: label,
    tools: true,
    paid: false,
    tier: rank,
    describe: "#{label}, a capable model",
    price: nil,
    context_window: window,
    echo: "never",
    usable: true,
    switch_drops_thinking: false,
    health: {verdict: "success", age_ms: 12000}
  }
end

def example_paid(label, rank, out)
  put_all(
    example_route(label, rank, 200_000),
    [
      [:provider, "anthropic"],
      [:paid, true],
      [:price, {"in" => 3, "out" => out, "cache" => 0.3}]
    ]
  )
end

def example_pool()
  [
    example_route("gpubox", 1, 32768),
    example_paid("sonnet", 2, 15),
    example_paid("opus", 3, 75)
  ]
end

def example_knobs(allow_paid)
  {
    judge: "judge",
    min_confidence: 70,
    stick_turns: 3,
    cooldown_secs: 300,
    allow_paid:
  }
end

def example_base(routes)
  {
    v: 1,
    prompt: "fix the failing test",
    tail: join_lines(["user: hi", "assistant: hello"]),
    turn: 7,
    last_turn_tool_calls: 0,
    context: {
      last_prompt_tokens: 18234,
      last_output_tokens: 812,
      prompt_chars: 340,
      max_tokens: 8192
    },
    cache_hit_ratio: 0.82,
    current: "gpubox",
    current_source: "auto",
    configured_primary: "gpubox",
    last_served: {label: "gpubox", fallback: false, latency_ms: 2310},
    routes:,
    knobs: example_knobs(false),
    memory: {},
    now_ms: 0
  }
end

def example_facts(routes, changes)
  facts_of(json_stringify(put_all(example_base(routes), changes)))
end

def example_paid_facts(changes)
  example_facts(example_pool, append([[:knobs, example_knobs(true)]], changes))
end

def example_mass(i, level)
  if i == level
    1.0
  else
    0.0
  end
end

def example_certain(level)
  reduce(
    (acc, i) -> assoc(acc, to_s(i), example_mass(i, level - 1)),
    {},
    range(0, 5)
  )
end

def example_judgment(level, c, s, t, pick, pick_conf, level_conf)
  answers = {
    route: {type: "choice", choice: pick, confidence: pick_conf},
    continues: {type: "noul", noul: c},
    tools: {type: "noul", noul: t},
    sensitive: {type: "noul", noul: s},
    difficulty: {
      type: "score",
      score: level - 1,
      probabilities: example_certain(level),
      confidence: level_conf
    }
  }
  read(200, json_stringify({model: "judge-1", answers:}))
end

def example_level(level, c, s)
  example_judgment(level, c, s, 0.9, nil, 0, 0.9)
end

def example_overflow()
  [
    [
      :context,
      {
        last_prompt_tokens: 30000,
        last_output_tokens: 812,
        prompt_chars: 340,
        max_tokens: 8192
      }
    ]
  ]
end

test "an empty pool, unreadable facts and an empty object all abstain"
  assert facts_of("not json").nil?
  assert facts_of("[1, 2]").nil?
  assert facts_of(nil).nil?
  blank_facts = facts_of("{}")
  assert blank_facts[:routes] == []
  assert blank_facts[:current_source] == "auto"
  assert decide(blank_facts, nil, nil)[:route].nil?
  f = example_facts([], [])
  d = decide(f, nil, {})
  assert d[:route].nil?
  assert d[:why] == "no route is eligible: stay"
  assert eligible(f, nil) == []
  assert questions(f, eligible(f, nil)).nil?
end

test "a single eligible current route abstains, judged or not"
  f = example_facts([example_route("gpubox", 1, 32768)], [])
  assert labels(eligible(f, nil)) == ["gpubox"]
  assert questions(f, eligible(f, nil)).nil?
  assert judge_request(f).nil?
  assert decide(f, nil, {})[:route].nil?
  hard = example_level(5, 0.0, 0.0)
  assert decide(f, hard, {})[:route].nil?
  assert decide(f, hard, {})[:why] == "gpubox already fits (difficulty 5)"
end

test "the context fits within 0.9 of the window, counting a third of the prompt's characters"
  w = example_route("w", 1, 10000)
  at = fn(chars, top)
    example_facts(
      [w],
      [
        [:current, "w"],
        [
          :context,
          {
            last_prompt_tokens: 0,
            last_output_tokens: 0,
            prompt_chars: chars,
            max_tokens: top
          }
        ]
      ]
    )
  end
  assert labels(eligible(at(3, 8999), nil)) == ["w"]
  assert labels(eligible(at(6, 8999), nil)) == []
  assert labels(eligible(at(0, 9001), nil)) == []
  assert labels(
    eligible(example_facts([example_route("gpubox", 1, 32768)], []), nil)
  ) == ["gpubox"]
  assert demand(example_facts([], [])) == 18234 + 812 + 340 / 3 + 8192
  open = example_facts(
    [example_route("open", 1, nil)],
    [
      [:current, "open"],
      [
        :context,
        {
          last_prompt_tokens: 900_000,
          last_output_tokens: 0,
          prompt_chars: 0,
          max_tokens: 0
        }
      ]
    ]
  )
  assert labels(eligible(open, nil)) == ["open"]
end

test "context overflow moves the floor to the nearest route at least as strong"
  pool = [
    example_route("tiny", 0, 1_000_000),
    example_route("gpubox", 1, 32768),
    example_route("huge", 3, 1_000_000),
    example_route("big", 1, 1_000_000)
  ]
  d = decide(example_facts(pool, example_overflow), nil, {})
  assert d[:route] == "big"
  assert d[:why] ==
    "the floor moves off an ineligible route: gpubox (the context does not fit) to big"
  assert d[:memory][:last_choice] == "big"
  assert d[:memory][:since_switch] == 0
  assert d[:memory][:switches] == 1
  top = [
    example_route("tiny", 0, 1_000_000),
    example_route("gpubox", 2, 32768),
    example_route("big", 1, 1_000_000)
  ]
  assert decide(example_facts(top, example_overflow), nil, {})[:route] == "big"
end

test "the floor never picks a no-tools route, and a judged light task may"
  phone = put_all(example_route("phone", 2, 1_000_000), [[:tools, false]])
  f = example_facts(
    [phone, example_route("gpubox", 1, 32768)],
    example_overflow
  )
  assert eligible(f, nil) == []
  assert labels(askable(f)) == ["phone"]
  assert decide(f, nil, {})[:route].nil?
  light = example_judgment(1, 0.0, 0.0, 0.1, nil, 0, 0.9)
  assert decide(f, light, {})[:route] == "phone"
  tooled = example_judgment(1, 0.0, 0.0, 0.5, nil, 0, 0.9)
  assert decide(f, tooled, {})[:route].nil?
end

test "unauthorized is never eligible, and a failing route sits out its cooldown"
  sick = fn(verdict, age)
    put_all(example_route("r", 1, nil), [[:health, {verdict:, age_ms: age}]])
  end
  only = (r) -> labels(eligible(example_facts([r], [[:current, "r"]]), nil))
  assert only(sick("unauthorized", 1_000_000_000)) == []
  assert only(sick("throttled", 299_999)) == []
  assert only(sick("throttled", 300_000)) == ["r"]
  assert only(sick("timed-out", 0)) == []
  assert only(sick("server-error", 299_999)) == []
  assert only(sick("unreachable", 299_999)) == []
  assert only(sick("unknown", 0)) == ["r"]
  assert only(put_all(example_route("r", 1, nil), [[:health, nil]])) == ["r"]
end

test "with allow_paid off only the configured primary may be paid"
  pool = [example_route("gpubox", 1, 32768), example_paid("sonnet", 2, 15)]
  assert labels(eligible(example_facts(pool, []), nil)) == ["gpubox"]
  assert labels(
    eligible(example_facts(pool, [[:configured_primary, "sonnet"]]), nil)
  ) == %w[gpubox sonnet]
  assert labels(
    eligible(example_facts(pool, [[:knobs, example_knobs(true)]]), nil)
  ) == %w[gpubox sonnet]
  unpriced = put_all(example_route("x", 1, nil), [[:paid, nil]])
  assert labels(eligible(example_facts([unpriced], []), nil)) == []
end

test "a manual choice pins the route, and /route auto adopts it"
  f = example_paid_facts([[:current, "sonnet"], [:current_source, "manual"]])
  light = example_level(1, 0.0, 0.0)
  d = decide(f, light, {last_choice: "gpubox"})
  assert d[:route].nil?
  assert d[:why] == "you chose the route"
  assert d[:memory][:mode] == "pin"
  again = decide(f, light, d[:memory])
  assert again[:why] == "the route is pinned"
  stored = json_stringify(again[:memory])
  auto = overlay(again[:memory], object_of(command_of("auto", stored)[:write]))
  resumed = decide(f, light, auto)
  assert resumed[:route] == "gpubox"
  assert resumed[:memory][:adopt] == false
  assert resumed[:memory][:mode] == "auto"
  assert decide(f, light, {last_choice: "sonnet"})[:route] == "gpubox"
end

test "stickiness holds for stick_turns, and only a confident hard upgrade breaks it"
  f = example_paid_facts([])
  middling = example_level(3, 0.0, 0.0)
  held = decide(f, middling, {since_switch: 1})
  assert held[:route].nil?
  assert held[:why] == "sticky: 1 of 3 turns since the last switch"
  assert held[:memory][:since_switch] == 2
  assert decide(f, middling, {since_switch: 3})[:route] == "sonnet"
  assert decide(f, middling, {})[:route] == "sonnet"
  hard = (level, conf) -> example_judgment(level, 0.0, 0.0, 0.9, nil, 0, conf)
  assert decide(f, hard(5, 0.9), {since_switch: 1})[:route] == "opus"
  assert decide(f, hard(4, 0.9), {since_switch: 0})[:route] == "opus"
  assert decide(f, hard(5, 0.8), {since_switch: 1})[:route].nil?
  down = example_paid_facts([[:current, "opus"]])
  assert decide(down, hard(1, 0.99), {since_switch: 1})[:route].nil?
end

test "a forced route is honoured once, and an ineligible one is named and dropped"
  f = example_paid_facts([])
  d = decide(f, nil, {forced: "opus"})
  assert d[:route] == "opus"
  assert d[:why] == "you asked for opus"
  assert d[:memory][:forced].nil?
  assert d[:memory][:last_choice] == "opus"
  assert decide(f, nil, d[:memory])[:route].nil?
  assert decide(f, nil, {forced: "opus", mode: "pin"})[:route] == "opus"
  refused = decide(example_facts(example_pool, []), nil, {forced: "opus"})
  assert refused[:route].nil?
  assert refused[:why] == "@opus is not eligible: paid"
  assert refused[:memory][:forced].nil?
  assert decide(f, nil, {forced: "nope"})[:why] ==
    "@nope is not a configured route"
end

test "a directive is @label and a space, and a known label once labels are known"
  assert directive_of("@opus explain this", %w[gpubox opus]) ==
    ["opus", "explain this"]
  assert directive_of("@opus   explain", ["opus"]) == %w[opus explain]
  assert directive_of("@nope explain", %w[gpubox opus]).nil?
  assert directive_of("@src/main.rs explain", nil).nil?
  assert directive_of("@gpubox explain", nil) == %w[gpubox explain]
  assert directive_of("@opus", ["opus"]).nil?
  assert directive_of("ask @opus now", ["opus"]).nil?
  assert directive_of("@ opus", nil).nil?
  assert known_labels("gpubox opus") == %w[gpubox opus]
  stored = forcing(nil, "opus")
  assert fields_of(json_parse(stored)) == ["forced"]
  assert memory_of(json_parse(stored)) ==
    put_all(memory_of(nil), [[:forced, "opus"]])
end

test "control: a blind or refused judgment decides exactly as no judge"
  f = example_paid_facts([])
  m = {since_switch: 5}
  none = decide(f, nil, m)
  shifted = decide(f, example_level(3, 0.0, 0.0), m)
  same = (d) -> [d[:route], d[:why], d[:memory]]
  assert none[:route].nil?
  assert shifted[:route] == "sonnet"
  assert same(decide(f, read(503, ""), m)) == same(none)
  assert same(decide(f, read(400, "{}"), m)) == same(none)
  assert same(decide(f, read(200, "{}"), m)) == same(none)
  assert decide(f, read(503, ""), m)[:judged][:kind] == "blind"
  assert same(shifted) != same(none)
end

test "a low-confidence route choice falls back to the difficulty's candidate"
  f = example_paid_facts([])
  pick = (conf) -> example_judgment(3, 0.0, 0.0, 0.9, "opus", conf, 0.9)
  assert decide(f, pick(0.5), {})[:route] == "sonnet"
  assert decide(f, pick(0.69), {})[:route] == "sonnet"
  assert decide(f, pick(0.9), {})[:route] == "opus"
  assert decide(f, pick(0.9), {})[:why] ==
    "the judge picked opus (difficulty 3 of 5)"
end

test "continues blocks a downgrade mid-task, and so does a sensitive task"
  f = example_paid_facts([[:current, "opus"]])
  assert decide(f, example_level(1, 0.8, 0.0), {})[:why] ==
    "no downgrade mid-task"
  assert decide(f, example_level(1, 0.5, 0.0), {})[:route].nil?
  assert decide(f, example_level(1, 0.1, 0.1), {})[:route] == "gpubox"
  guarded = example_judgment(1, 0.0, 0.6, 0.9, "sonnet", 0.9, 0.9)
  assert decide(f, guarded, {})[:why] == "no downgrade mid-task"
end

test "a continuation keeps its cache unless the task turned hard"
  f = example_paid_facts([])
  assert decide(f, example_level(3, 0.8, 0.0), {})[:why] ==
    "continuation, cache kept"
  assert decide(f, example_level(4, 0.8, 0.0), {})[:route] == "opus"
  assert decide(f, example_level(3, 0.2, 0.0), {})[:route] == "sonnet"
end

test "a judge more than one tier from the difficulty's candidate is overruled"
  f = example_paid_facts([[:current, "opus"]])
  far = example_judgment(1, 0.0, 0.0, 0.9, "opus", 0.95, 0.9)
  near = example_judgment(1, 0.0, 0.0, 0.9, "sonnet", 0.95, 0.9)
  assert decide(f, far, {})[:route] == "gpubox"
  assert decide(f, near, {})[:route] == "sonnet"
end

test "a tools need of 0.2 or more keeps the on-device route out"
  phone = put_all(example_route("phone", 0, 8192), [[:tools, false]])
  f = example_facts(
    [phone, example_route("gpubox", 1, 32768)],
    [
      [
        :context,
        {
          last_prompt_tokens: 0,
          last_output_tokens: 0,
          prompt_chars: 10,
          max_tokens: 1024
        }
      ]
    ]
  )
  tooled = (t) -> example_judgment(1, 0.0, 0.0, t, nil, 0, 0.9)
  assert labels(eligible(f, tooled(0.2))) == ["gpubox"]
  assert decide(f, tooled(0.2), {})[:route].nil?
  assert labels(eligible(f, tooled(0.19))) == %w[phone gpubox]
  assert decide(f, tooled(0.19), {})[:route] == "phone"
end

test "the route question offers exactly the eligible routes, by their descriptions"
  f = example_paid_facts([])
  e = eligible(f, nil)
  qs = questions(f, e)
  options_of = last(first(qs))[:options]
  assert map((q) -> first(q), qs) ==
    %w[route continues tools sensitive difficulty]
  assert size(e) == 3
  assert size(options_of) == size(e)
  assert map((o) -> first(o), options_of) == %w[gpubox sonnet opus]
  assert last(first(options_of)) == "gpubox, a capable model"
  assert size(last(last(qs))[:levels]) == 5
  sent = json_parse(request("judge-latest", state_of(f), qs))
  assert size(
    json_get(json_get(json_get(sent, :questions), :route), :criteria)
  ) == 3
  assert questions(f, [first(e)]).nil?
  bare = put_all(example_route("bare", 1, nil), [[:describe, ""]])
  assert options([bare]) == [%w[bare bare]]
  assert state_of(f) ==
    join_lines(
      [
        "Earlier conversation (oldest first, truncated):",
        "user: hi",
        "assistant: hello",
        "",
        "Newest message:",
        "fix the failing test"
      ]
    )
  assert judge_request(f) != nil
  unjudged = example_paid_facts(
    [[:knobs, put_all(example_knobs(true), [[:judge, nil]])]]
  )
  assert judge_request(unjudged).nil?
end

test "an answer round-trips through json_parse, memory and all"
  f = example_paid_facts([])
  d = decide(f, example_level(3, 0.0, 0.0), {decisions: 2})
  doc = json_parse(answer(d))
  assert json_get(doc, :route) == "sonnet"
  assert json_get(doc, :why) == d[:why]
  assert memory_of(json_get(doc, :memory)) == d[:memory]
  assert d[:memory][:decisions] == 3
  assert json_get(json_get(doc, :judged), :difficulty) == 3.0
  kept = json_parse(answer(decide(f, nil, {})))
  assert json_get(kept, :route).nil?
  assert json_get(kept, :judged).nil?
  assert memory_of(json_parse(json_stringify(memory_of(nil)))) == memory_of(nil)
end

test "strength puts an untiered route below every tier, then free, cheap, dear, then declaration"
  r = fn(label, rank, paid, out)
    put_all(
      example_route(label, rank, nil),
      [[:paid, paid], [:price, {"out" => out}]]
    )
  end
  pool = [
    r("a", nil, true, 10),
    r("b", nil, false, nil),
    r("c", 1, false, nil),
    r("d", nil, true, 5),
    r("e", 1, true, 3),
    r("f", 1, false, nil),
    r("g", nil, true, nil)
  ]
  f = example_facts(pool, [[:knobs, example_knobs(true)]])
  assert labels(strength(f[:routes])) == %w[b d a g c f e]
end

test "difficulty maps onto the eligible tiers by rounding halves up, and sensitivity takes the top"
  assert map((d) -> rung_index(d, 0.0, 3), [1, 2, 3, 4, 5]) == [0, 1, 1, 2, 2]
  assert map((d) -> rung_index(d, 0.0, 5), [1, 2, 3, 4, 5]) == [0, 1, 2, 3, 4]
  assert rung_index(5, 0.0, 1) == 0
  assert rung_index(1, 0.5, 3) == 2
  assert rung_index(1, 0.49, 3) == 0
  assert rung_index(9, 0.0, 3) == 2
  assert rung_index(-3, 0.0, 3) == 0
end

test "a switch that would drop thinking mid-tool-run is excluded, staying put is not"
  think = put_all(
    example_route("think", 1, nil),
    [[:switch_drops_thinking, true], [:echo, "when-tools"]]
  )
  pool = [example_route("gpubox", 1, 32768), think]
  assert labels(
    eligible(example_facts(pool, [[:last_turn_tool_calls, 5]]), nil)
  ) == ["gpubox"]
  assert labels(
    eligible(example_facts(pool, [[:last_turn_tool_calls, 0]]), nil)
  ) == %w[gpubox think]
  assert labels(
    eligible(
      example_facts(pool, [[:last_turn_tool_calls, 5], [:current, "think"]]),
      nil
    )
  ) == %w[gpubox think]
  quiet = put_all(think, [[:echo, "never"]])
  assert labels(
    eligible(example_facts([quiet], [[:last_turn_tool_calls, 5]]), nil)
  ) == ["think"]
end

test "memory counts decisions, switches and abstains by reason"
  f = example_paid_facts([])
  one = decide(f, nil, {})[:memory]
  two = decide(f, nil, one)[:memory]
  three = decide(f, example_level(3, 0.0, 0.0), two)[:memory]
  assert three[:decisions] == 3
  assert three[:switches] == 1
  assert three[:abstains] == {"no_judgment" => 2}
  assert explain(three) ==
    join_lines(
      [
        "mode: auto",
        "last: difficulty 3 of 5: sonnet",
        "decisions: 3, switches: 1",
        "abstained: no_judgment 2"
      ]
    )
  assert explain(memory_of(nil)) ==
    join_lines(
      [
        "mode: auto",
        "last: no decision yet",
        "decisions: 0, switches: 0",
        "abstained: none"
      ]
    )
end

test "recall lays the stored memory over the kernel's echo, field by field"
  echo = put_all(memory_of(nil), [[:decisions, 9], [:last_choice, "gpubox"]])
  assert recall(nil, echo) == echo
  assert recall("not json", echo) == echo
  assert recall("{\"forced\":\"opus\"}", echo) ==
    put_all(echo, [[:forced, "opus"]])
  assert recall(json_stringify(memory_of(nil)), echo) == memory_of(nil)
  assert recall(
    "{\"forced\":null,\"mode\":\"bogus\"}",
    put_all(echo, [[:forced, "x"], [:mode, "pin"]])
  ) == echo
end

test "/route sets only the mode, and why shows the last decision"
  m = decide(example_paid_facts([]), example_level(3, 0.0, 0.0), {})[:memory]
  stored = json_stringify(m)
  pinned = command_of("pin", stored)
  assert overlay(m, object_of(pinned[:write])) == put_all(m, [[:mode, "pin"]])
  assert pinned[:hint] == "route pinned: the current route stays"
  assert fields_of(object_of(command_of("off", nil)[:write])) == %w[adopt mode]
  shown = command_of("why", stored)
  assert shown[:write].nil?
  assert shown[:panel] ==
    join_lines(
      [
        "mode: auto",
        "last: difficulty 3 of 5: sonnet",
        "decisions: 1, switches: 1",
        "abstained: none"
      ]
    )
  assert command_of("bogus", stored)[:hint] ==
    "route policy auto: /route auto | pin | off | why"
end

test "a judge reply is read through chokkan, and a transport failure is blind"
  assert judgment(nil).nil?
  assert get(
    judgment("{\"status\":0,\"verdict\":\"unreachable\",\"ms\":3,\"body\":\"\"}"),
    :kind
  ) == :blind
  assert judgment("garbage")[:kind] == :blind
  body = json_stringify(
    {model: "judge-1", answers: {tools: {type: "noul", noul: 0.1}}}
  )
  ok = judgment(
    json_stringify({status: 200, verdict: "success", ms: 120, body:})
  )
  assert ok[:kind] == :answered
  assert tools_need(ok) == 0.1
end

test "the exports are the choose-route hook, the @label directive and /route"
  assert map((r) -> take(3, r), rows(exports)) ==
    [
      %w[hook choose-route choose],
      %w[hook before-submit directive],
      %w[command route route]
    ]
end
