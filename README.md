# AgentHarbor

Persistent, isolated workspaces for agent teams. AgentHarbor is organized as a
monorepo containing a Tauri desktop shell, a Rust orchestration core, and
versioned TypeScript contracts and CLI adapters.

## Workspace

- `apps/desktop`: Tauri 2 + React/TypeScript Windows desktop application.
- `crates/orchestrator`: process, PTY, worktree, filesystem, approval, and
  persistence boundary.
- `packages/protocol`: versioned UI/orchestrator commands, events, and errors.
- `packages/agent-adapters`: adapters that translate private agent CLI output
  into the public protocol.
- `docs/architecture`: system design and security assumptions.

## Development

```sh
npm install
npm run typecheck
cargo test --workspace
```

Run the web frontend with `npm run dev --workspace @agentharbor/desktop`, or
the complete desktop application with `npm run tauri --workspace
@agentharbor/desktop -- dev` on a machine with the Tauri prerequisites.
