# Hybrid artifact (repo-forge THEORY §VI.3): generated from repo-forge.lisp,
# then hand-extended past what that spec can express. `repo-forge migrate`
# skips it; `migrate --include-hybrid` would REVERT it — and reintroduce the
# bug the generator produced: it rendered option names (:model/:theme) as
# lisp keys, which ArnesConfig never declared, so every setting was
# silently dropped. Owed upstream in repo-forge before the spec can own this
# file again: nullable int/bool/enum/float option types and a per-option
# lisp-key override (HmOptionTypeAst, repo-forge-ast/src/hm_module.rs).
{ self }:
{ config, lib, pkgs, options, ... }:

let
  cfg = config.programs.arnes;
  # anvil is the fleet's one MCP-server declaration; when it is loaded,
  # arnes registers as an agent and reads the file anvil renders for it —
  # the same servers every other agent gets, declared once.
  hasAnvil = options ? blackmatter && options.blackmatter ? components
    && options.blackmatter.components ? anvil;
  anvilMcpPath = ".config/arnes/mcp.json";
  useAnvil = cfg.mcpFromAnvil && hasAnvil;
  mcpJson =
    if cfg.mcpJson != null then cfg.mcpJson
    else if useAnvil then "${config.home.homeDirectory}/${anvilMcpPath}"
    else null;
  inherit (lib) mkEnableOption mkOption mkIf types optionalString;
  pkg =
    if cfg.package != null then cfg.package
    else self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  # Keys mirror ArnesConfig's serde names (arnes-config/src/lib.rs); the
  # crate's `parses_the_form_the_hm_module_renders` test pins this shape.
  # One renderer per value shape; null / empty → the key is omitted, so the
  # provider default applies. Floats/ints/strings are JSON-compatible atoms.
  kv = k: v: optionalString (v != null) "\n  :${k} ${builtins.toJSON v}";
  kvBool = k: v: optionalString (v != null) "\n  :${k} ${if v then "#t" else "#f"}";
  kvList = k: vs: optionalString (vs != [ ]) "\n  :${k} (${lib.concatMapStringsSep " " builtins.toJSON vs})";
  kvHeaders = hs: optionalString (hs != { }) "\n  :extra-headers (${
    lib.concatStringsSep " " (lib.mapAttrsToList (n: v: "(:name ${builtins.toJSON n} :value ${builtins.toJSON v})") hs)
  })";
  # One keyword list per fallback, the same key names as the top level.
  kvFallbacks = fs: optionalString (fs != [ ]) "\n  :fallbacks (${
    lib.concatMapStringsSep "\n              " (f: "(:provedor ${builtins.toJSON f.provider}${
      kv "modelo" f.model}${kv "endpoint" f.endpoint}${kv "api-key-file" f.apiKeyFile}${
      kv "key-env" f.keyEnv}${kv "timeout-secs" f.timeoutSecs}${kv "system-note" f.systemNote}${
      kvBool "tools" f.tools}${kv "label" f.label}${
      let r = renderSection routeFacts f.facts; in optionalString (r != "") " :facts ${r}"})") fs
  })";
  providers = [ "local" "deepseek" "anthropic" "messages" ];
  routeFacts = {
    auth = leaf (types.enum [ "x-api-key" "bearer" ]) "How the key is sent. null → the provider's: bearer for `messages`, x-api-key for the rest.";
    describe = leaf types.str "What the model is good at, in a sentence: the text a route judge reads when it picks a route.";
    tier = leaf (between 0 9) "Strength rank among the routes, 0 the lightest. null → ranked by price.";
    priceIn = leaf types.number "USD per million input tokens.";
    priceOut = leaf types.number "USD per million output tokens.";
    priceCache = leaf types.number "USD per million cache-read tokens.";
    contextWindow = leaf types.ints.positive "The model's context window in tokens.";
  };
  nullOr = t: mkOption' (types.nullOr t);
  mkOption' = type: description: mkOption { inherit type description; default = null; };

  # ── Sections (docs/configuration.md) ──────────────────────────────────
  # One spec per section, and every option and every rendered key comes
  # from it: `leaf` is one typed, nullable knob (null = the built-in, and the
  # key is left out of the file); a plain attrset is a nested section. The
  # crate's `every_section_leaf_has_a_typed_option` test reads this file and
  # fails when a key the schema declares has no leaf here.
  leaf = type: description: { __leaf = true; inherit type description; };
  onOff = types.enum [ "on" "off" ];
  between = lo: hi: types.ints.between lo hi;
  keyEdits = {
    bind = leaf (types.attrsOf types.str) "Chord → action id (`{ \"ctrl+q\" = \"app:exit\"; }`), applied after unbind; each entry refused alone.";
    unbind = leaf (types.listOf types.str) "Chords removed from the defaults.";
  };
  sections = {
    ui = {
      theme = leaf (types.enum fleetThemes) "Fleet theme (wins over `theme`).";
      motion = leaf onOff "off draws every surface at its end state (reduced motion).";
      notifications = leaf (types.enum [ "auto" "off" ]) "Desktop notifications; off = bell only (wins over `notifications`).";
      editorMode = leaf (types.enum [ "normal" "vim" ]) "Prompt editing (wins over `editorMode`).";
      regions = {
        noticeRows = leaf (between 0 10) "Most rows the notice lane opens to. null → 3.";
        pickerRows = leaf (between 3 40) "Rows a picker claims above the prompt. null → 12.";
      };
      notices = {
        infoSecs = leaf (between 1 600) "Seconds an info notice stays. null → 4.";
        warnSecs = leaf (between 1 600) "Seconds a warning stays. null → 8.";
        errorSecs = leaf (between 1 600) "Seconds an error stays. null → 12.";
      };
      timing = {
        frameRate = leaf (between 1 240) "Most frames a second while something moves. null → 60.";
        idlePollMs = leaf (between 10 1000) "Input poll interval when idle, ms. null → 50.";
        revealRowMs = leaf (between 0 200) "A finished reply unrolls this many ms per row. null → 12.";
      };
    };
    keys = {
      prompt = keyEdits;
      nav = {
        menu = keyEdits;
        filter = keyEdits;
      };
    };
    activity = {
      shimmerMs = leaf (between 200 10000) "One sweep of the thinking shimmer, ms. null → 1800.";
      holdMs = leaf (between 100 10000) "How long a tool tick stays lit, ms. null → 1200.";
      notifyAfterSecs = leaf (between 0 3600) "Turns shorter than this finish without a notification. null → 10.";
    };
    mcp = {
      exposure = leaf (types.enum [ "auto" "all" "deferred" ]) "How MCP tools reach the model (wins over `mcpTools`).";
      inlineMax = leaf (between 1 2000) "Under auto, tools sent inline up to this many. null → 40.";
      connectTimeoutSecs = leaf (between 1 300) "A server's start and tool listing, seconds. null → 15.";
      callTimeoutSecs = leaf (between 1 3600) "One tool call, seconds. null → 120.";
      startup = leaf (types.enum [ "background" "block" ]) "background: the TUI draws at once and servers attach as they come up; block: wait for all. null → background.";
      lazy = leaf (types.listOf types.str) "Globs of servers not started until their first call.";
      only = leaf (types.listOf types.str) "Globs of the servers to use; empty = every discovered one.";
      off = leaf (types.listOf types.str) "Globs of servers never started.";
      scopes = leaf (types.listOf types.str) "anvil scopes whose files (scopeDir/<scope>.json) are read; the module renders one per `mcpScopes` entry.";
      scopeDir = leaf types.str "Where the scope files live. null → ~/.config/arnes/mcp.";
      project = leaf types.bool "Read ./.mcp.json in the working directory. null → off.";
      health = {
        enabled = leaf onOff "Probe and reconnect servers. null → on.";
        probeMinMs = leaf (between 1000 3600000) "First interval once up, ms. null → 15000.";
        probeMaxMs = leaf (between 1000 86400000) "Ceiling for a healthy server, ms. null → 300000.";
        growthPct = leaf (between 100 1000) "Healthy interval growth per ok probe, percent. null → 200.";
        failMs = leaf (between 100 600000) "First retry after a failure, ms. null → 2000.";
        reconnectMaxMs = leaf (between 1000 3600000) "Ceiling of the reconnect backoff, ms. null → 120000.";
        jitterPct = leaf (between 0 100) "Jitter per interval, percent. null → 10.";
        quarantineFlaps = leaf (between 0 100) "Flips inside the window that quarantine a server; 0 = never. null → 4.";
        quarantineWindowSecs = leaf (between 1 86400) "null → 300.";
        quarantineSecs = leaf (between 1 86400) "How long a quarantine lasts. null → 600.";
        timeoutMs = leaf (between 100 60000) "One probe's answer, ms. null → 2000.";
        lease = leaf onOff "Share routine probes with other arnes processes through vigy.db. null → on.";
      };
    };
    persistence = {
      sessions = leaf onOff "Record each TUI session so it can be resumed. null → on.";
      sessionsKeep = leaf types.ints.unsigned "Saved sessions kept, newest first; 0 = unbounded. null → 200.";
      sessionsMaxAgeDays = leaf types.ints.unsigned "Saved sessions older than this are removed; 0 = never. null → 90.";
      logMaxMb = leaf types.ints.positive "A log past this many MiB is rotated at startup. null → 8.";
      ledger = leaf types.str "Usage ledger path, or \"off\" (wins over `ledger`).";
    };
    freeRoute = {
      enabled = leaf types.bool "Start on the free local route when the provider cannot serve. null → on.";
      endpoint = leaf types.str "The llama.cpp endpoint. null → http://127.0.0.1:8080.";
      onDevice = leaf types.bool "Add Apple's on-device model after it. null → on on macOS.";
      onDeviceEndpoint = leaf types.str "null → http://127.0.0.1:8434.";
      onDeviceModel = leaf types.str "null → apple-on-device.";
    };
    routing = {
      enabled = leaf types.bool "Ask the hosted choose-route hooks for each new prompt's route. null → off.";
      budgetMs = leaf (between 100 5000) "Wall-clock budget for the choice, judge call included. null → 1000.";
      tailKib = leaf (between 1 64) "How much of the conversation's tail the choice sees. null → 16.";
      log = leaf types.str "Decision log path, or \"off\". null → $XDG_STATE_HOME/arnes/routes.jsonl.";
      judge = leaf types.str "The endpoint a choose-route hook may ask (an `endpoints` name). null → none: the floor alone decides.";
      minConfidence = leaf (between 0 100) "Below this a judged pick gives way to the floor. null → 70.";
      stickTurns = leaf (between 0 50) "Turns a switch holds before another. null → 3.";
      cooldownSecs = leaf (between 0 86400) "How long a failing route sits out. null → 300.";
      allowPaid = leaf types.bool "Whether a paid route other than the configured primary may be chosen. null → off.";
    };
    route = {
      label = leaf types.str "The primary route's name in notices, the status line and route picks. null → the provider's name.";
      tools = leaf types.bool "Whether the primary route is sent the tool list. null → yes.";
      facts = routeFacts;
    };
    catalog = {
      maxAgeHours = leaf (between 0 720) "Hours a cached model catalogue is trusted. null → 24.";
    };
    mentions = {
      maxFiles = leaf (between 1 1000000) "Files the @ picker lists. null → 20000.";
      maxKib = leaf (between 1 16384) "Largest file an @ mention attaches, KiB. null → 256.";
    };
    history = {
      record = leaf onOff "Keep prompts on disk and recall them in later sessions. null → on.";
      scope = leaf (types.enum [ "project-then-global" "project" "global" ]) "Which directories' prompts Up/Down and Ctrl-R reach. null → project-then-global.";
      keep = leaf (between 0 1000000) "Prompts kept on disk, newest first; 0 = unbounded. null → 10000.";
      search = leaf (types.enum [ "picker" "incremental" ]) "Ctrl-R: the ranked fuzzy picker or reverse-i-search. null → picker.";
      ranking = leaf (types.enum [ "frecency" "recency" ]) "The picker's order. null → frecency.";
      pinRecent = leaf types.bool "The most recent prompt stays first in the picker. null → on.";
    };
    turn = {
      maxRounds = leaf (between 1 256) "Most model calls in one turn. null → 16.";
    };
    skills = {
      enabled = leaf types.bool "Load Claude-Code-style SKILL.md skills. null → on.";
      dirs = leaf (types.listOf types.str) "Directories of skill directories. Empty → ~/.claude/skills and ./.claude/skills.";
      only = leaf (types.listOf types.str) "Globs of skill names to load; empty = all.";
      off = leaf (types.listOf types.str) "Globs of skill names never loaded.";
      maxListed = leaf (between 0 2000) "Most skills listed to the model. null → 200.";
    };
    context = {
      files = leaf (types.listOf types.str) "Files appended to the system prompt under their path.";
      discover = leaf types.bool "Read CLAUDE.md and AGENTS.md in the working directory. null → on.";
      prompt = leaf types.str "A fragment appended after the files.";
      maxKib = leaf (between 1 4096) "Largest context file read, KiB. null → 64.";
    };
    access = {
      readRoots = leaf (types.listOf types.str) "Directories arnes_read_file and @path may read under; empty = anywhere.";
      keyFiles = leaf (types.listOf types.str) "API key files a route may read; empty = any.";
    };
  };
  # A profile's overlay: every section above plus the flat keys a profile
  # usually changes, rendered as the profile's `:config` plist (docs/profiles.md).
  profileFlat = {
    provedor = leaf (types.enum providers) "The provider while this profile is active.";
    modelo = leaf types.str "The model while this profile is active.";
    promptSistema = leaf types.str "The system prompt while this profile is active.";
    systemNote = leaf types.str "The route note while this profile is active.";
    permissionMode = leaf (types.enum [ "default" "plan" "bypass" ]) "The permission mode while this profile is active.";
    allowTools = leaf (types.listOf types.str) "Tool globs that run without asking.";
    denyTools = leaf (types.listOf types.str) "Tool globs that never run.";
    modulesOff = leaf (types.listOf types.str) "Hosted bidama entries turned off.";
    bidamas = leaf (types.listOf types.str) "Bidamas hosted beside arnes's own.";
    builtinTools = leaf types.bool "The built-in tools.";
  };
  profileSpec = sections // profileFlat;
  kvProfiles = optionalString (cfg.profiles != { }) "\n  :profiles (${
    lib.concatStringsSep "\n             " (lib.mapAttrsToList (name: p: "(:name ${builtins.toJSON name}${
      kv "description" p.description}${kvList "inherits" p.inherits}${
      let r = renderSection profileSpec p.config; in optionalString (r != "") " :config ${r}"})") cfg.profiles)
  })";
  # Blue extensions: lists of declarations, one typed submodule each. An
  # entry the crate cannot use (a missing :fn, a bad schema) is refused by
  # itself; the others load.
  decls = {
    commands = {
      name = leaf types.str "The slash command's name (one word).";
      description = leaf types.str "The / picker's one-line description.";
      blue = leaf types.str "The .b file (~ expands).";
      fn = leaf types.str "The function in it to call.";
    };
    tools = {
      name = leaf types.str "The tool's name.";
      description = leaf types.str "What the model is told it does.";
      blue = leaf types.str "The .b file.";
      fn = leaf types.str "The function; it receives the input as JSON text.";
      schema = leaf types.str "The input JSON Schema, as JSON text. null → an object with no declared properties.";
      readOnly = leaf types.bool "Whether it changes nothing. null → it may, so it is asked.";
    };
    hooks = {
      on = leaf (types.enum [ "before-submit" "after-turn" "before-tool" ]) "The event.";
      blue = leaf types.str "The .b file.";
      fn = leaf types.str "The function.";
    };
    vigies = {
      name = leaf types.str "The vigy's name (one word).";
      blue = leaf types.str "The .b file.";
      fn = leaf types.str "The function; it receives the MCP probe facts as JSON text.";
      everyMs = leaf types.ints.positive "Tick interval, ms. null → 5000.";
    };
    endpoints = {
      name = leaf types.str "The name a choose-route hook passes to arnes_http.";
      url = leaf types.str "The URL arnes_http POSTs to.";
      auth = leaf (types.enum [ "x-api-key" "bearer" ]) "How the key is sent. null → bearer.";
      keyFile = leaf types.str "File holding the key (inside access.key-files). The key never reaches blue.";
      keyEnv = leaf types.str "Env var holding the key when keyFile is null.";
      timeoutMs = leaf (types.ints.between 100 10000) "Request timeout. null → 700.";
      maxKib = leaf (types.ints.between 1 4096) "Largest reply handed back. null → 256.";
    };
  };
  kvDecls = lib.concatStrings (lib.mapAttrsToList (name: spec:
    optionalString (cfg.${name} != [ ]) "\n  :${kebab name} (${lib.concatMapStringsSep " " (renderSection spec) cfg.${name}})") decls);
  mkSectionOptions = spec: lib.mapAttrs (name: v:
    if v ? __leaf then mkOption' (types.nullOr v.type) v.description
    else mkOption {
      type = types.submodule { options = mkSectionOptions v; };
      default = { };
      description = "The `${name}` section (docs/configuration.md).";
    }) spec;
  # camelCase option name → the lisp keyword shikumi lowers to snake_case.
  kebab = name: lib.concatMapStrings (c: if lib.toLower c != c then "-${lib.toLower c}" else c) (lib.stringToCharacters name);
  lispAtom = v:
    if builtins.isBool v then (if v then "#t" else "#f")
    else if builtins.isList v then "(${lib.concatMapStringsSep " " lispAtom v})"
    else if builtins.isAttrs v then "(${lib.concatStringsSep " " (lib.mapAttrsToList (k: x: "(${builtins.toJSON k} ${builtins.toJSON x})") v)})"
    else builtins.toJSON v;
  # A section's set values as `(:k v …)`, or "" when nothing in it is set.
  renderSection = spec: value:
    let
      parts = lib.concatLists (lib.mapAttrsToList (name: v:
        let x = value.${name}; in
        if v ? __leaf then lib.optional (x != null && x != [ ] && x != { }) ":${kebab name} ${lispAtom x}"
        else let inner = renderSection v x; in lib.optional (inner != "") ":${kebab name} ${inner}") spec);
    in if parts == [ ] then "" else "(${lib.concatStringsSep " " parts})";
  kvSections = lib.concatStrings (lib.mapAttrsToList (name: spec:
    let r = renderSection spec cfg.${name}; in optionalString (r != "") "\n  :${kebab name} ${r}") sections);
  fleetThemes = [ "dark" "pleme_dark" "pleme-dark" "vellum" "polar_veil" "polar-veil" "bare" "nord" ];
in
{
  options.programs.arnes = {
    enable = mkEnableOption "arnes — Rust/tatara-lisp open-source Claude-Code-equivalent agent harness";

    package = mkOption {
      type = types.nullOr types.package;
      default = null;
      description = "Override the default package.";
    };

    provider = mkOption {
      type = types.enum providers;
      default = "local";
      description = "Model provider. All speak the Anthropic Messages wire format; `local` (the default, free) is a llama.cpp llama-server (no key; set `endpoint`). A paid provider is used once it is set here or in `fallbacks` with its key.";
    };

    systemNote = nullOr types.str "Appended to the system prompt while the primary route answers: what the model is and where it runs. Travels with the route, so /model to another route drops it.";

    fallbacks = mkOption {
      type = types.listOf (types.submodule {
        options = {
          provider = mkOption { type = types.enum providers; description = "The route's provider."; };
          model = nullOr types.str "Model ID. null → the provider's default (local: `local`).";
          endpoint = nullOr types.str "Base URL. null → the provider's.";
          apiKeyFile = nullOr types.str "File holding the key. null → the provider's env var (a local server needs none).";
          keyEnv = nullOr types.str "Env var holding the key when apiKeyFile is null.";
          timeoutSecs = nullOr types.ints.positive "Whole-request timeout for this route. null → the primary's.";
          systemNote = nullOr types.str "Appended to the system prompt when this route answers: what the model is and where it runs (a model knows nothing of its host and will invent one).";
          tools = nullOr types.bool "Whether this route is sent the tool list. null → yes. Off for a small-context model (Apple's on-device model has 4096 tokens).";
          label = nullOr types.str "The route's name in notices and the status line. null → the provider's name (two `local` routes need telling apart).";
          facts = mkOption {
            type = types.submodule { options = mkSectionOptions routeFacts; };
            default = { };
            description = "The route's facts: auth, description, tier, prices, context window.";
          };
        };
      });
      default = [ ];
      example = lib.literalExpression ''[ { provider = "local"; endpoint = "http://192.0.2.10:8080"; } ]'';
      description = ''
        Routes tried in order when the provider cannot serve a turn (unreachable,
        out of credit, unauthorised, rate limited, failing), and started on when
        its key cannot be read. A route whose own key cannot be read is skipped
        by name; the others still serve.
      '';
    };

    model = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "deepseek-reasoner";
      description = "Model ID, checked against the provider's live catalogue at startup. null → the provider's default (deepseek-flash / claude-opus-5-5).";
    };

    apiKeyFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/Users/me/.config/deepseek/api-key";
      description = "File holding the API key (e.g. a sops-nix secret path). null → the provider's env var.";
    };

    theme = mkOption {
      type = types.enum fleetThemes;
      default = "dark";
      description = "Fleet theme (ishou-tokens): dark is pleme_dark; vellum, polar_veil, bare; nord is a preset name. `ui.theme` wins when set.";
    };

    maxTokens = mkOption {
      type = types.int;
      default = 8192;
      description = "Default max_tokens per request.";
    };

    systemPrompt = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Optional system prompt injected at conversation start.";
    };

    endpoint = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Override the provider's base URL (e.g. for proxies).";
    };

    keyEnv = nullOr types.str "Env var holding the key when apiKeyFile is null. null → DEEPSEEK_API_KEY / ANTHROPIC_API_KEY.";
    apiVersion = nullOr types.str "`anthropic-version` header. null → 2023-06-01.";

    # ── Sampling — null = provider default, omitted from the request ──
    temperature = nullOr (types.numbers.between 0 2) "Sampling temperature, 0–2.";
    topP = nullOr (types.numbers.between 0 1) "Nucleus sampling, 0–1.";
    topK = nullOr types.ints.positive "Top-k sampling.";
    stopSequences = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Strings that end generation.";
    };

    # ── Reasoning ──
    thinking = nullOr (types.enum [ "on" "off" ]) "Reasoning on/off. null → provider default (DeepSeek: on, effort high); setting effort implies on.";
    effort = nullOr (types.enum [ "low" "high" "max" ]) "Reasoning effort (output_config.effort). `low` is the cheap default for routine agent steps.";
    thinkingBudget = nullOr types.ints.positive "Thinking token budget — honoured by Anthropic only; DeepSeek ignores it (use effort).";
    echoThinking = nullOr types.bool "Re-send prior thinking blocks. null → provider rule (DeepSeek: no, Anthropic: yes).";

    # ── Transport ──
    timeoutSecs = nullOr types.ints.positive "Whole-request timeout in seconds.";
    connectTimeoutSecs = nullOr (types.ints.between 1 120) "Seconds a route may take to accept a connection before the next route gets the turn. null → 10.";
    maxRetries = nullOr types.ints.unsigned "Retries on 429/5xx. null → 3.";
    retryBaseMs = nullOr types.ints.positive "First retry backoff (ms), doubling per attempt. null → 500.";
    extraHeaders = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = { "x-trace" = "on"; };
      description = "Extra HTTP headers on every request.";
    };

    mcpFromAnvil = mkOption {
      type = types.bool;
      default = true;
      description = "Register arnes as a blackmatter anvil agent and load the MCP servers anvil renders for it (no-op where anvil is not loaded).";
    };

    guardrail = mkOption {
      type = types.nullOr (types.enum [ "on" "off" ]);
      default = null;
      description = "Screen Bash commands and Write/Edit content with guardrail's production rule set (the one the Claude Code hook enforces). Block = refused in every mode; Warn = always asked. null → on.";
    };

    builtinTools = mkOption {
      type = types.nullOr types.bool;
      default = null;
      description = "Built-in tools (Read/Write/Edit/Glob/Grep/Bash). null → on. Read/Glob/Grep run unasked; the rest go through the permission gate.";
    };

    ledger = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "off";
      description = "Usage ledger (forge-core cost events, one per API call): a path, or \"off\". null → $XDG_STATE_HOME/arnes/usage.jsonl.";
    };

    notifications = mkOption {
      type = types.nullOr (types.enum [ "auto" "off" ]);
      default = null;
      description = "Desktop notifications (OSC 9/777/99 + bell) when approval is needed or a turn longer than 10s finishes. off = bell only.";
    };

    editorMode = mkOption {
      type = types.nullOr (types.enum [ "normal" "vim" ]);
      default = null;
      description = "Prompt editing: normal (Claude Code / readline keys) or vim (escriba's vim — Normal/Command via escriba's key pipeline, :w submits, :q quits; Insert keeps the Claude Code keys).";
    };

    permissionMode = mkOption {
      type = types.nullOr (types.enum [ "default" "plan" "bypass" ]);
      default = null;
      description = "default: ask before tools that may write; plan: read-only tools only; bypass: never ask. Shift-Tab cycles in the TUI.";
    };

    allowTools = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "mcp__codesearch__*" ];
      description = "Tool-name globs that run without asking.";
    };

    denyTools = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Tool-name globs that never run (deny beats allow).";
    };

    mcpTools = mkOption {
      type = types.nullOr (types.enum [ "auto" "all" "deferred" ]);
      default = null;
      description = "How MCP tools reach the model. null/auto: inline up to 40, else two search/call meta-tools; all: every schema every turn; deferred: always meta-tools.";
    };

    mcpScopes = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "pleme" "ops" ];
      description = ''
        anvil scopes rendered as ~/.config/arnes/mcp/<scope>.json (through anvil's
        serversForScope), for `mcp.scopes` and profiles to name. Every scope a
        profile's `mcp.scopes` lists belongs here too.
      '';
    };

    mcpScope = mkOption {
      type = types.nullOr types.str;
      default = "pleme";
      description = ''
        anvil scope whose servers arnes launches (servers declare `scopes`).
        Every server starts on every launch and its tool schemas ride on every
        request, so a scope keeps both bounded. null → register as a plain
        anvil agent and take every server.
      '';
    };

    mcpJson = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Explicit anvil-format MCP file ({\"mcpServers\": …}). Overrides the anvil-rendered one.";
    };

    modulesOff = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "help" ];
      description = "Hosted bidama entries (commands, segments) turned off by name (docs/kernel.md). Off is not deleted: see kernelFallback.";
    };
    kernelFallback = nullOr types.bool "Serve a feature from its Rust twin when its bidama entry is off. null → no.";
    moduleBudgetMs = nullOr types.ints.positive "Wall-clock budget for one blue command or segment call, ms. null → 250.";
    profile = nullOr types.str "The profile used when --profile, this directory's last choice and the choose-profile hook name none (docs/profiles.md).";
    profiles = mkOption {
      type = types.attrsOf (types.submodule {
        options = {
          description = nullOr types.str "The /profile picker's one line.";
          inherits = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = "Parent profiles applied first, left to right.";
          };
          config = mkOption {
            type = types.submodule { options = mkSectionOptions profileSpec; };
            default = { };
            description = "The overlay: any section, and the flat keys a profile usually changes; each value refused alone when it does not fit.";
          };
        };
      });
      default = { };
      example = lib.literalExpression ''{ ops = { inherits = [ "base" ]; config = { permissionMode = "plan"; mcp.only = [ "codesearch" ]; }; }; }'';
      description = "Named config overlays, switched live with /profile or --profile (docs/profiles.md).";
    };
    bidamas = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "mine" ];
      description = "Bidamas to host beside arnes's own (meirei, joutai), by name, each resolved on arnes's bidama path (docs/kernel.md).";
    };
  } // mkSectionOptions sections // lib.mapAttrs (name: spec: mkOption {
    type = types.listOf (types.submodule { options = mkSectionOptions spec; });
    default = [ ];
    description = "Blue ${name} (docs/programmability.md), one declaration each.";
  }) decls // {
    extraConfig = mkOption {
      type = types.lines;
      default = "";
      example = '':extras (:escriba-socket "/tmp/escriba.sock")'';
      description = "Raw keyword pairs appended inside the (defarnes …) form — the escape hatch for knobs without a typed option. Every key in docs/configuration.md has a typed option.";
    };
  };

  config = mkIf cfg.enable (lib.mkMerge [{
    home.packages = [ pkg ];

    home.file.".config/arnes/arnes.lisp".text = ''
      ;; ~/.config/arnes/arnes.lisp — generated by home-manager.
      (defarnes
        :provedor ${builtins.toJSON cfg.provider}
        :tema ${builtins.toJSON cfg.theme}
        :max-tokens ${toString cfg.maxTokens}${kv "modelo" cfg.model}${kv "api-key-file" cfg.apiKeyFile}${kv "key-env" cfg.keyEnv}${kv "api-version" cfg.apiVersion}${kv "prompt-sistema" cfg.systemPrompt}${kv "endpoint" cfg.endpoint}${kv "temperatura" cfg.temperature}${kv "top-p" cfg.topP}${kv "top-k" cfg.topK}${kvList "stop-sequences" cfg.stopSequences}${kv "thinking" cfg.thinking}${kv "effort" cfg.effort}${kv "thinking-budget" cfg.thinkingBudget}${kvBool "echo-thinking" cfg.echoThinking}${kv "timeout-secs" cfg.timeoutSecs}${kv "connect-timeout-secs" cfg.connectTimeoutSecs}${kv "max-retries" cfg.maxRetries}${kv "retry-base-ms" cfg.retryBaseMs}${kvHeaders cfg.extraHeaders}${kvFallbacks cfg.fallbacks}${kv "anvil-json" mcpJson}${kv "mcp-tools" cfg.mcpTools}${kv "editor-mode" cfg.editorMode}${kv "notifications" cfg.notifications}${kv "ledger" cfg.ledger}${kvBool "builtin-tools" cfg.builtinTools}${kv "guardrail" cfg.guardrail}${kv "permission-mode" cfg.permissionMode}${kvList "allow-tools" cfg.allowTools}${kvList "deny-tools" cfg.denyTools}${kv "system-note" cfg.systemNote}${kvList "modules-off" cfg.modulesOff}${kvBool "kernel-fallback" cfg.kernelFallback}${kv "module-budget-ms" cfg.moduleBudgetMs}${kvList "bidamas" cfg.bidamas}${kv "profile" cfg.profile}${kvProfiles}${kvSections}${kvDecls}${optionalString (cfg.extraConfig != "") "\n  ${cfg.extraConfig}"})
    '';

    home.sessionVariables = {
      ARNES_CONFIG = "${config.home.homeDirectory}/.config/arnes/arnes.lisp";
    };
    # Scoped: the file is rendered through anvil's own filter — one server
    # declaration, no second list here.
    home.file.${anvilMcpPath} = mkIf (useAnvil && cfg.mcpScope != null && cfg.mcpJson == null) {
      text = builtins.toJSON {
        mcpServers = config.blackmatter.components.anvil.serversForScope cfg.mcpScope;
      };
    };
  }
  (lib.optionalAttrs hasAnvil {
    # One file per declared scope, read by `mcp.scopes` (docs/profiles.md).
    home.file = lib.listToAttrs (map (scope: {
      name = ".config/arnes/mcp/${scope}.json";
      value.text = builtins.toJSON { mcpServers = config.blackmatter.components.anvil.serversForScope scope; };
    }) (lib.optionals cfg.mcpFromAnvil cfg.mcpScopes));
  })
  (lib.optionalAttrs hasAnvil {
    # Unscoped: a plain anvil agent registration (every server).
    blackmatter.components.anvil.agents.arnes = mkIf (cfg.mcpFromAnvil && cfg.mcpScope == null) {
      enable = true;
      configPath = anvilMcpPath;
      configFormat = "mcpjson";
    };
  })]);
}
