import type { Event } from "@agentharbor/protocol";

/** Keeps vendor-specific parsing outside both the webview and orchestrator API. */
export interface AgentAdapter {
  readonly id: string;
  executable(environment: "windows" | "wsl2"): string;
  arguments(prompt?: string): readonly string[];
  consume(sessionId: string, chunk: Uint8Array): readonly Event[];
  finish(sessionId: string, exitCode: number | null): readonly Event[];
}

export class PlainTerminalAdapter implements AgentAdapter {
  readonly id = "plain-terminal";
  readonly #decoder = new TextDecoder();

  executable(environment: "windows" | "wsl2"): string {
    return environment === "windows" ? "powershell.exe" : "/bin/bash";
  }

  arguments(prompt?: string): readonly string[] {
    return prompt ? ["-c", prompt] : [];
  }

  consume(sessionId: string, chunk: Uint8Array): readonly Event[] {
    return [{ type: "session.output", sessionId, stream: "terminal", text: this.#decoder.decode(chunk, { stream: true }) }];
  }

  finish(sessionId: string, exitCode: number | null): readonly Event[] {
    return [{ type: "session.state", sessionId, state: "exited", ...(exitCode === null ? {} : { exitCode }) }];
  }
}
