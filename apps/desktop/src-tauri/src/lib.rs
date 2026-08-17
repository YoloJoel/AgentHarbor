use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PROTOCOL_VERSION: u8 = 1;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    version: u8,
    request_id: String,
    payload: Value,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
    Success { ok: bool, value: Value },
    Failure { ok: bool, error: Value },
}

#[tauri::command]
fn dispatch(message: Envelope) -> Response {
    if message.version != PROTOCOL_VERSION {
        return Response::Failure {
            ok: false,
            error: json!({
                "code": "PROTOCOL_VERSION_UNSUPPORTED", "message": "Unsupported protocol version",
                "retryable": false, "details": { "requestId": message.request_id }
            }),
        };
    }
    match message.payload.get("type").and_then(Value::as_str) {
        Some("workspace.list") => Response::Success {
            ok: true,
            value: json!({ "workspaces": [] }),
        },
        _ => Response::Failure {
            ok: false,
            error: json!({
                "code": "INVALID_COMMAND", "message": "Command is not implemented", "retryable": false
            }),
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![dispatch])
        .run(tauri::generate_context!())
        .expect("error while running AgentHarbor");
}
