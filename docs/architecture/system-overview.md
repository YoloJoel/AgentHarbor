# System overview

## Process boundaries

AgentHarbor has three deliberate trust and failure boundaries:

1. **Webview UI** — React runs in the Tauri webview. It renders normalized
   events and sends versioned commands; it never parses an agent CLI's private
   stdout format and cannot start arbitrary processes directly.
2. **Tauri host** — the Rust host exposes a small IPC command surface. It
   validates protocol versions and arguments, asks the approval service about
   privileged operations, then delegates to the orchestrator crate. It owns
   application lifecycle but contains no agent-specific parsing.
3. **Execution children** — Claude Code, AgentCode, shells, and Git execute as
   child processes behind adapter and PTY boundaries. Each child has an
   explicit workspace, environment allowlist, cancellation handle, and output
   stream. A child is never trusted merely because it was launched locally.

The shared `packages/protocol` contract is the only interface visible to the
UI. `packages/agent-adapters` converts vendor output into that contract.

## Requirement specification lifecycle

The lead agent begins intake with read-only repository analysis. Observed
facts, explicitly labelled inferences, and de-duplicated questions are stored
separately. Architecture-changing questions come first, followed by questions
that change acceptance, then delivery details.

Each requirement specification follows an OpenSpec-style structure: goal,
background, in/out of scope, constraints, assumptions, user scenarios,
acceptance criteria, risks, and open questions. The same content is stored as
machine-readable database fields and as
`.agentharbor/specs/<requirement-id>/v<version>.md` in the source repository.
After user confirmation, freezing changes the immutable version's state and
records a SHA-256 hash of the exact Markdown artifact.

Execution plans, worker tasks, and final acceptance records copy both the
frozen specification id and hash. A change creates a successor version rather
than editing history; outstanding tasks are marked `review_required` until
their impact is resolved. The event stream records analysis, version creation,
and freezing for audit and recovery.

## Failure recovery

The orchestrator writes workspace metadata, approvals, and session state using
atomic replace (temporary file, flush, then rename). On launch it loads the
last valid snapshot, marks sessions whose recorded PID is no longer owned by
AgentHarbor as interrupted, validates worktree paths, and offers an explicit
resume or clean-up action. Corrupt records are quarantined rather than
silently overwritten.

Child exit, malformed adapter output, PTY closure, and protocol mismatch become
typed protocol errors or events. They do not crash the host. Event sequence
numbers let the UI detect a gap and request a fresh snapshot. Cancellation is
graceful first and forceful after a deadline. Git operations are serialized per
repository so recovery cannot race a new worktree mutation.

## Data directories

Machine-local state uses the platform application-data directory returned by
Tauri (normally `%APPDATA%\\AgentHarbor` on Windows):

```text
AgentHarbor/
  state/          # versioned metadata snapshots and session journals
  logs/           # redacted host and child diagnostics
  approvals/      # durable approval decisions and audit records
  cache/          # disposable adapter and discovery data
```

Source repositories and Git worktrees remain outside this directory at paths
chosen by the user. Secrets are stored in the operating-system credential
store, not in snapshots or logs. Cache deletion must not lose workspace state.

## Execution environment

Every session records its executable, argument vector, working directory,
adapter kind, environment kind, and sanitized environment overrides. The
orchestrator uses argument arrays rather than shell-concatenated strings. The
default environment inherits only a documented allowlist (for example PATH,
HOME/USERPROFILE, locale, proxy, and terminal variables); tokens are injected
through the credential service and redacted from telemetry.

The PTY service owns terminal dimensions, input, output backpressure, resize,
and shutdown. Filesystem access resolves canonical paths and enforces a
workspace-root capability. Git worktrees are created and removed through a
repository-scoped service, never by UI-supplied command text.

## Windows and WSL2 boundary

Native Windows execution and WSL2 execution are distinct environment kinds.
Windows children receive Windows paths and run through ConPTY. WSL2 children
are launched through a selected distribution and receive Linux paths translated
at the boundary; UNC `\\wsl$` paths are not passed to Linux tools as if they
were native paths. The session record stores both the display path and the
canonical path for its execution environment.

The host does not assume Windows and WSL2 share processes, credentials, PATH,
file watching semantics, case sensitivity, line endings, or Git configuration.
Cross-boundary file access is opt-in because it is slower and can weaken file
permission expectations. A WSL distribution stopping is handled as execution
environment loss, not as an empty workspace.

## Approval and security assumptions

Agent CLIs and repository contents are untrusted. Before execution, the host
intercepts operations classified as process launch, write outside the granted
root, destructive filesystem mutation, Git worktree mutation, credential use,
or network access. Approval requests show normalized executable/arguments,
target paths, environment, risk, and whether a decision is one-shot or scoped.
Adapters cannot approve their own operations.

The initial release assumes the signed desktop host, local OS account, Tauri
runtime, Rust orchestrator, and protocol package are trusted. It does **not**
claim to sandbox an approved child from all resources available to that OS
account. OS ACLs, Windows sandboxing, WSL isolation, and enterprise policy are
defense-in-depth. IPC accepts only known command variants and protocol versions;
webview navigation and content-security policy are restricted to bundled
assets. Audit records redact secrets but preserve actor, scope, decision, and
time.
