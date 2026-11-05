# Context parity with Claude Code

Status: PLAN, 2026-11-05. Not started.

arnes reads instruction files the way Claude Code does, and reads `AGENTS.md`
as well. Today `system_prompt` (`arnes/src/main.rs`) reads only `./CLAUDE.md`
and `./AGENTS.md` in the working directory, then `context.files`.

## What Claude Code does

Source: code.claude.com/docs/en/memory.md, context-window.md, hooks.md,
settings-reference.md, env-vars.md (read 2026-11-05).

| # | Behaviour | arnes today |
|---|---|---|
| 1 | Levels, broad to specific, concatenated: managed (`/Library/Application Support/ClaudeCode/CLAUDE.md`, `/etc/claude-code/CLAUDE.md`), user `~/.claude/CLAUDE.md`, project `./CLAUDE.md` or `./.claude/CLAUDE.md`, local `./CLAUDE.local.md` after `CLAUDE.md` in the same directory | cwd only |
| 2 | The working directory and every ancestor, root first | no walk |
| 3 | `CLAUDE.md` and `CLAUDE.local.md` in a subdirectory load when a file in that subtree is read, written or edited | none |
| 4 | `@path` imports: relative to the importing file, absolute and `~` allowed, at most 4 hops, not inside code spans or fenced blocks, `\ ` escapes a space, a quoted path is not an import; the first external import in a project asks for approval; block HTML comments are stripped | `@path` only in prompts (`sanshou`) |
| 5 | `.claude/rules/**/*.md` (user rules first): no `paths:` frontmatter loads at launch; `paths:` globs load when Read, Write or Edit touches a match; bad YAML loads as unscoped | none |
| 6 | `AGENTS.md` is read only when no `CLAUDE.md` or `CLAUDE.local.md` exists in cwd or above; the recommended form is `@AGENTS.md` inside `CLAUDE.md` | always read beside `CLAUDE.md` |
| 7 | `claudeMdExcludes` globs on absolute paths (managed cannot be excluded); `--add-dir` directories load their `CLAUDE.md` only with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`; `CLAUDE_CODE_DISABLE_CLAUDE_MDS=1` turns all off; launch files whole up to 4 MiB, on-demand files capped (40k characters on a 200K window); a warning over about 200 lines | `context.discover`, `context.max-kib` |
| 8 | Delivered as a user message after the system prompt, in `<system-reminder>`, with a lead line that the instructions override default behaviour; each file labelled `Contents of <path> (<scope>):`, an import right after its importer; the project-root file is read again after compaction | in the system prompt |
| 9 | Auto memory: `MEMORY.md`, first 200 lines or 25 KB, at session start | none |
| 10 | Also at start: git status snapshot, environment (cwd, platform, shell, OS), SessionStart and UserPromptSubmit `additionalContext`; the `InstructionsLoaded` hook reports each file with `load_reason` | none |

## Plan

1. A bidama owns the policy: which files, in what order, imports, rules and
   their globs, the AGENTS.md fallback, excludes and caps. It reads through
   `arnes_read_file` and needs two scoped host words: list a directory, and
   the cwd and home. Name it with `/naming`.
2. The kernel calls it at start and again on a tool read, write or edit (rows
   3 and 5), and sends its result as a user-message reminder (row 8).
3. Every row is a `context` knob in `arnes-config` with its default, bound and
   `module/default.nix` leaf (`docs/configuration.md`).
4. Proof: render and resolution tests over a fixture tree, one per row.
