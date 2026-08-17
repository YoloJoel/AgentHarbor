/** The major version changes whenever a message shape becomes incompatible. */
export const PROTOCOL_VERSION = 1 as const;
export type ProtocolVersion = typeof PROTOCOL_VERSION;

export type ExecutionEnvironment =
  | { kind: "windows" }
  | { kind: "wsl2"; distribution: string };

export interface Envelope<T> {
  version: ProtocolVersion;
  requestId: string;
  payload: T;
}

export type Command =
  | { type: "workspace.list" }
  | { type: "workspace.open"; path: string; environment: ExecutionEnvironment }
  | { type: "session.start"; workspaceId: string; adapter: string; prompt?: string }
  | { type: "session.input"; sessionId: string; data: string }
  | { type: "session.resize"; sessionId: string; columns: number; rows: number }
  | { type: "session.cancel"; sessionId: string }
  | { type: "approval.resolve"; approvalId: string; decision: "allow-once" | "deny" };

export interface WorkspaceSummary {
  id: string;
  displayName: string;
  path: string;
  environment: ExecutionEnvironment;
}

export type Event =
  | { type: "workspace.snapshot"; workspaces: WorkspaceSummary[] }
  | { type: "session.state"; sessionId: string; state: "starting" | "running" | "interrupted" | "exited"; exitCode?: number }
  | { type: "session.output"; sessionId: string; stream: "terminal" | "diagnostic"; text: string }
  | { type: "approval.requested"; approvalId: string; sessionId: string; operation: string; risk: "low" | "medium" | "high"; details: Record<string, string> };

export type ErrorCode =
  | "PROTOCOL_VERSION_UNSUPPORTED"
  | "INVALID_COMMAND"
  | "APPROVAL_REQUIRED"
  | "ACCESS_DENIED"
  | "EXECUTION_ENVIRONMENT_UNAVAILABLE"
  | "PROCESS_FAILED"
  | "PERSISTENCE_FAILED"
  | "INTERNAL";

export interface ProtocolError {
  code: ErrorCode;
  message: string;
  retryable: boolean;
  details?: Record<string, string>;
}

export type Response<T = unknown> =
  | { ok: true; value: T }
  | { ok: false; error: ProtocolError };

export function envelope<T>(requestId: string, payload: T): Envelope<T> {
  return { version: PROTOCOL_VERSION, requestId, payload };
}

export function isCompatibleEnvelope(value: unknown): value is Envelope<unknown> {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as Partial<Envelope<unknown>>;
  return candidate.version === PROTOCOL_VERSION &&
    typeof candidate.requestId === "string" && "payload" in candidate;
}
