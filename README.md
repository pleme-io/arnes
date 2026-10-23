# arnes

An agent harness for the terminal, in the style of Claude Code, written in Rust with a blue and tatara-lisp configuration and scripting layer.

It talks to the Anthropic API or to any compatible endpoint, runs tools and MCP servers, keeps conversations in a local store, and renders a TUI. Routes (a primary model plus fallbacks), profiles, hooks and scheduled checks are declared in one config file.

## Documentation

| Topic | File |
|---|---|
| Design | `docs/design.md` |
| Configuration, routes and endpoints | `docs/configuration.md` |
| Profiles | `docs/profiles.md` |
| Scripting in blue | `docs/programmability.md` |
| Persistence | `docs/persistence.md` |
| Integrations | `docs/integrations.md` |

## Build

```
nix run .#arnes -- --help
cargo test --workspace
```

## License

MIT.
