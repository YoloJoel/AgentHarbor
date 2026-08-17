import React from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { envelope, type Command, type Response } from "@agentharbor/protocol";
import "./styles.css";

async function send(command: Command): Promise<Response> {
  return invoke("dispatch", { message: envelope(crypto.randomUUID(), command) });
}

function App() {
  const [status, setStatus] = React.useState("Orchestrator ready");
  const refresh = async () => {
    try {
      const response = await send({ type: "workspace.list" });
      setStatus(response.ok ? "Workspace snapshot synchronized" : response.error.message);
    } catch {
      setStatus("Desktop bridge unavailable (browser preview)");
    }
  };

  return <main>
    <header><div className="mark">AH</div><div><h1>AgentHarbor</h1><p>Persistent workspaces for agent teams</p></div></header>
    <section className="hero">
      <div><span className="eyebrow">LOCAL ORCHESTRATION</span><h2>Your agents.<br/>Safely in harbor.</h2>
      <p>Isolated Git worktrees, durable sessions, and an approval boundary for every agent command.</p>
      <button onClick={refresh}>Refresh workspaces</button></div>
      <aside><span>System status</span><strong>{status}</strong><ol><li>Versioned IPC protocol</li><li>Windows + WSL2 execution</li><li>Approval-gated operations</li></ol></aside>
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<React.StrictMode><App /></React.StrictMode>);
