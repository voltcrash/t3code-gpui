//! Orchestration commands the desktop sends, mirroring
//! `packages/client-runtime/src/operations/commands.ts`.
//!
//! Only the server-resolved forms are implemented: the server picks steer,
//! queue or restart for a follow-up and finds the run to stop. Servers that
//! predate them do not advertise `serverResolvedCommandContext` and
//! `threadStop`, and the composer stays read-only against them.

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{RpcClient, RpcError};

pub const DISPATCH: &str = "orchestration.dispatchCommand";

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
