//! Orchestration commands the desktop sends, mirroring
//! `packages/client-runtime/src/operations/commands.ts`.
//!
//! Only the server-resolved forms are implemented: the server picks steer,
//! queue or restart for a follow-up and finds the run to stop. Servers that
//! predate them do not advertise `serverResolvedCommandContext` and
//! `threadStop`, and the composer stays read-only against them.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{RpcClient, RpcError};

pub const DISPATCH: &str = "orchestration.dispatchCommand";
pub const LAUNCH_THREAD: &str = "orchestration.launchThread";

/// The desktop shell has always identified as the web client.
const CREATION_SOURCE: &str = "web";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResult {
    pub sequence: u64,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// `startThreadTurn` with `dispatchMode: "auto"` and no attachments.
pub fn send_message(thread_id: &str, text: &str) -> Value {
    json!({
        "type": "message.dispatch",
        "commandId": new_id(),
        "createdBy": "user",
        "creationSource": CREATION_SOURCE,
        "threadId": thread_id,
        "messageId": new_id(),
        "text": text,
        "attachments": [],
        "deliveryIntent": "auto",
        "dispatchMode": { "type": "start_immediately" },
    })
}

/// `interruptThreadTurn` without a run: the server stops whatever is running.
pub fn stop_thread(thread_id: &str) -> Value {
    json!({
        "type": "thread.stop",
        "commandId": new_id(),
        "threadId": thread_id,
    })
}

/// `respondToThreadApproval`. `feedback` goes with a decline the provider
/// can explain to the agent.
pub fn respond_to_approval(
    thread_id: &str,
    request_id: &str,
    decision: &str,
    feedback: Option<&str>,
) -> Value {
    let mut command = json!({
        "type": "runtime-request.respond",
        "commandId": new_id(),
        "threadId": thread_id,
        "requestId": request_id,
        "decision": decision,
    });
    if let Some(feedback) = feedback {
        command["feedback"] = feedback.into();
    }
    command
}

/// `respondToThreadUserInput`, keyed by question id.
pub fn respond_to_user_input(
    thread_id: &str,
    request_id: &str,
    answers: Map<String, Value>,
) -> Value {
    json!({
        "type": "runtime-request.respond",
        "commandId": new_id(),
        "threadId": thread_id,
        "requestId": request_id,
        "answers": answers,
    })
}

/// `dismissThreadUserInput`: closes an async question without answering it.
pub fn dismiss_user_input(thread_id: &str, request_id: &str) -> Value {
    json!({
        "type": "thread.user-input.dismiss",
        "commandId": new_id(),
        "threadId": thread_id,
        "requestId": request_id,
    })
}

/// `deriveThreadTitleSeed` for plain text: whitespace collapsed, then cut
/// at 50 characters like `truncate` in `packages/shared/src/String.ts`.
pub fn title_seed(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "New thread".into();
    }
    // `String.slice` counts UTF-16 code units.
    let units: Vec<u16> = normalized.encode_utf16().collect();
    if units.len() <= 50 {
        return normalized;
    }
    let cut = String::from_utf16_lossy(&units[..50]);
    format!("{}...", cut.trim_end_matches('\u{FFFD}'))
}

pub struct NewThread<'a> {
    pub project_id: &'a str,
    pub model_selection: &'a Value,
    pub runtime_mode: &'a str,
    pub text: &'a str,
}

/// `startThreadTurn` for a draft: `orchestration.launchThread` creates the
/// thread in the project's checkout and sends its first message.
pub fn launch_thread(thread: &NewThread<'_>) -> (String, Value) {
    let thread_id = new_id();
    let input = json!({
        "commandId": new_id(),
        "creationSource": CREATION_SOURCE,
        "threadId": thread_id,
        "projectId": thread.project_id,
        "title": title_seed(thread.text),
        "generateTitle": true,
        "modelSelection": thread.model_selection,
        "runtimeMode": thread.runtime_mode,
        "interactionMode": "default",
        "workspaceStrategy": { "type": "root" },
        "initialMessage": {
            "messageId": new_id(),
            "text": thread.text,
            "attachments": [],
        },
    });
    (thread_id, input)
}

pub async fn dispatch(rpc: &RpcClient, command: &Value) -> Result<DispatchResult, RpcError> {
    rpc.call(DISPATCH, command).await
}

/// Whether the server resolves the context of the commands above.
pub fn supported(config: &Value) -> bool {
    let capabilities = &config["environment"]["capabilities"];
    capabilities["serverResolvedCommandContext"] == true && capabilities["threadStop"] == true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_message_gets_fresh_command_and_message_ids() {
        let first = send_message("thread-1", "hi");
        let second = send_message("thread-1", "hi");
        assert_ne!(first["commandId"], second["commandId"]);
        assert_ne!(first["messageId"], second["messageId"]);
        assert_ne!(first["commandId"], first["messageId"]);
    }

    #[test]
    fn title_seeds_collapse_whitespace_and_truncate() {
        assert_eq!(title_seed("  fix   the\nbuild "), "fix the build");
        assert_eq!(title_seed("   "), "New thread");
        let long = "a".repeat(60);
        assert_eq!(title_seed(&long), format!("{}...", "a".repeat(50)));
    }

    #[test]
    fn support_needs_both_capabilities() {
        let config = |resolved: bool, stop: bool| {
            json!({ "environment": { "capabilities": {
                "serverResolvedCommandContext": resolved,
                "threadStop": stop,
            } } })
        };
        assert!(supported(&config(true, true)));
        assert!(!supported(&config(true, false)));
        assert!(!supported(&json!({})));
    }
}
