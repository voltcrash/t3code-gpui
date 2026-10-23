//! Effect RPC over a WebSocket with JSON serialization: the wire protocol the
//! server speaks in `apps/server/src/ws.ts`.
//!
//! Client frames are `Request`, `Ack`, `Interrupt`, and `Ping`; the server
//! answers with `Chunk`, `Exit`, `Defect`, and `Pong`. Streams are
//! flow-controlled: the server sends the next `Chunk` only after the client
//! acks the previous one.
//!
//! One task owns the socket. [`RpcClient`] handles only hold channels, so they
//! can be awaited from any executor (GPUI's included) while the socket itself
//! runs on tokio.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use url::Url;

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// Matches `PING_TIMEOUT` in `packages/client-runtime/src/rpc/protocol.ts`: a
/// server busy for a few seconds stops answering pings, so only a longer
/// silence drops the socket.
const SILENCE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcError {
    /// The handler failed with a typed domain error, still in its encoded form.
    #[error("request failed: {0}")]
    Failure(Value),
    #[error("server defect: {0}")]
    Defect(Value),
    #[error("request was interrupted")]
    Interrupted,
    #[error("could not connect: {0}")]
    Connect(Arc<str>),
    #[error("disconnected: {0}")]
    Disconnected(Arc<str>),
    #[error("could not decode {tag}: {message}")]
    Decode { tag: Arc<str>, message: Arc<str> },
}

impl RpcError {
    /// The `_tag` of a typed failure, e.g. `"OrchestrationDispatchCommandError"`.
    pub fn failure_tag(&self) -> Option<&str> {
        match self {
            RpcError::Failure(error) => error.get("_tag").and_then(Value::as_str),
            _ => None,
        }
    }
}

enum Reply {
    Once(oneshot::Sender<Result<Value, RpcError>>),
    Stream(mpsc::UnboundedSender<Result<Value, RpcError>>),
}

impl Reply {
    fn fail(self, error: RpcError) {
        match self {
            Reply::Once(tx) => {
                let _ = tx.send(Err(error));
            }
            Reply::Stream(tx) => {
                let _ = tx.send(Err(error));
            }
        }
    }
}

enum Command {
    Request {
        id: u64,
        tag: Arc<str>,
        payload: Value,
        reply: Reply,
    },
    Interrupt {
        id: u64,
    },
}

/// A cheap, cloneable handle to one authenticated socket.
#[derive(Clone)]
pub struct RpcClient {
    commands: mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicU64>,
    closed: watch::Receiver<Option<Arc<str>>>,
}

impl RpcClient {
    /// Opens the socket. Must run inside a tokio runtime, which keeps driving
    /// the connection after this returns.
    pub async fn connect(url: &Url) -> Result<Self, RpcError> {
        // Thread snapshots can be large; the server is the authority on size.
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let connecting =
            tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false);
        let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connecting)
            .await
            .map_err(|_| RpcError::Connect("timed out opening the WebSocket".into()))?
            .map_err(|error| RpcError::Connect(error.to_string().into()))?;

        let (commands, command_rx) = mpsc::unbounded_channel();
        let (closed_tx, closed) = watch::channel(None);
        tokio::spawn(async move {
            let reason = run(socket, command_rx).await;
            tracing::debug!(%reason, "rpc socket closed");
            let _ = closed_tx.send(Some(reason));
        });
        Ok(Self {
            commands,
            next_id: Arc::new(AtomicU64::new(0)),
            closed,
        })
    }

    /// Calls a request/response RPC.
    pub async fn call<I: Serialize, O: DeserializeOwned>(
        &self,
        tag: &str,
        input: &I,
    ) -> Result<O, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.send_request(tag, input, Reply::Once(tx))?;
        let value = rx
            .await
            .unwrap_or_else(|_| Err(self.disconnected_error()))?;
        decode(tag, value)
    }

    /// Starts a streaming RPC. Dropping the subscription interrupts it.
    pub fn subscribe<I: Serialize, T: DeserializeOwned>(
        &self,
        tag: &str,
        input: &I,
    ) -> Subscription<T> {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = match self.send_request(tag, input, Reply::Stream(tx.clone())) {
            Ok(id) => Some(id),
            Err(error) => {
                let _ = tx.send(Err(error));
                None
            }
        };
        Subscription {
            id,
            tag: tag.into(),
            items: rx,
            commands: self.commands.clone(),
            _item: PhantomData,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.borrow().is_some()
    }

    /// Resolves with the reason once the socket is gone.
    pub async fn closed(&self) -> Arc<str> {
        let mut closed = self.closed.clone();
        match closed.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or_else(|| "closed".into()),
            Err(_) => "closed".into(),
        }
    }

    fn send_request<I: Serialize>(
        &self,
        tag: &str,
        input: &I,
        reply: Reply,
    ) -> Result<u64, RpcError> {
        let payload = serde_json::to_value(input).map_err(|error| RpcError::Decode {
            tag: tag.into(),
            message: format!("encoding the payload: {error}").into(),
        })?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.commands
            .send(Command::Request {
                id,
                tag: tag.into(),
                payload,
                reply,
            })
            .map_err(|_| self.disconnected_error())?;
        Ok(id)
    }

    fn disconnected_error(&self) -> RpcError {
        let reason = self.closed.borrow().clone();
        RpcError::Disconnected(reason.unwrap_or_else(|| "closed".into()))
    }
}

/// Items of a streaming RPC. Ends after the server's final `Exit`; a failed
/// stream yields one error and then ends.
pub struct Subscription<T> {
    id: Option<u64>,
    tag: Arc<str>,
    items: mpsc::UnboundedReceiver<Result<Value, RpcError>>,
    commands: mpsc::UnboundedSender<Command>,
    _item: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Stream for Subscription<T> {
    type Item = Result<T, RpcError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.items.poll_recv(cx) {
            Poll::Ready(Some(item)) => {
                let tag = self.tag.clone();
                Poll::Ready(Some(item.and_then(|value| decode(&tag, value))))
            }
            Poll::Ready(None) => {
                self.id = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for Subscription<T> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.commands.send(Command::Interrupt { id });
        }
    }
}

fn decode<O: DeserializeOwned>(tag: &str, value: Value) -> Result<O, RpcError> {
    serde_json::from_value(value).map_err(|error| RpcError::Decode {
        tag: tag.into(),
        message: error.to_string().into(),
    })
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn run(socket: Socket, mut commands: mpsc::UnboundedReceiver<Command>) -> Arc<str> {
    let (mut sink, mut source) = socket.split();
    let mut pending: HashMap<u64, Reply> = HashMap::new();
    let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_frame = Instant::now();

    let reason: Arc<str> = loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    break "client closed".into();
                };
                let frame = match command {
                    Command::Request { id, tag, payload, reply } => {
                        pending.insert(id, reply);
                        json!({ "_tag": "Request", "id": id, "tag": &*tag, "payload": payload, "headers": [] })
                    }
                    Command::Interrupt { id } => {
                        if pending.remove(&id).is_none() {
                            continue;
                        }
                        json!({ "_tag": "Interrupt", "requestId": id })
                    }
                };
                if let Err(error) = sink.send(Message::text(frame.to_string())).await {
                    break error.to_string().into();
                }
            }
            message = source.next() => {
                let text = match message {
                    None => break "the server closed the connection".into(),
                    Some(Err(error)) => break error.to_string().into(),
                    Some(Ok(Message::Close(_))) => break "the server closed the connection".into(),
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(_)) => {
                        last_frame = Instant::now();
                        continue;
                    }
                };
                last_frame = Instant::now();
                let acks = match handle_frame(&text, &mut pending) {
                    Ok(acks) => acks,
                    Err(error) => break error,
                };
                let mut failed = None;
                for id in acks {
                    let ack = json!({ "_tag": "Ack", "requestId": id });
                    if let Err(error) = sink.send(Message::text(ack.to_string())).await {
                        failed = Some(error.to_string());
                        break;
                    }
                }
                if let Some(error) = failed {
                    break error.into();
                }
            }
            _ = ping.tick() => {
                if last_frame.elapsed() >= SILENCE_TIMEOUT {
                    break format!("the server stopped responding for {}s", SILENCE_TIMEOUT.as_secs()).into();
                }
                if let Err(error) = sink.send(Message::text(r#"{"_tag":"Ping"}"#)).await {
                    break error.to_string().into();
                }
            }
        }
    };

    for (_, reply) in pending.drain() {
        reply.fail(RpcError::Disconnected(reason.clone()));
    }
    let _ = sink.close().await;
    reason
}

/// Applies one server frame. Returns the stream ids to ack.
fn handle_frame(text: &str, pending: &mut HashMap<u64, Reply>) -> Result<Vec<u64>, Arc<str>> {
    let decoded: Value = serde_json::from_str(text)
        .map_err(|error| Arc::<str>::from(format!("unreadable server frame: {error}")))?;
    let messages = match decoded {
        Value::Array(messages) => messages,
        message => vec![message],
    };

    let mut acks = Vec::new();
    for message in messages {
        let request_id = message.get("requestId").and_then(parse_request_id);
        match message.get("_tag").and_then(Value::as_str) {
            Some("Chunk") => {
                let Some(id) = request_id else { continue };
                let Some(Reply::Stream(tx)) = pending.get(&id) else {
                    continue;
                };
                if let Some(Value::Array(values)) = message.get("values") {
                    for value in values {
                        let _ = tx.send(Ok(value.clone()));
                    }
                }
                acks.push(id);
            }
            Some("Exit") => {
                let Some(reply) = request_id.and_then(|id| pending.remove(&id)) else {
                    continue;
                };
                let exit = message.get("exit").cloned().unwrap_or(Value::Null);
                match (reply, exit_result(exit)) {
                    (Reply::Once(tx), result) => {
                        let _ = tx.send(result);
                    }
                    (Reply::Stream(_), Ok(_)) => {}
                    (Reply::Stream(tx), Err(error)) => {
                        let _ = tx.send(Err(error));
                    }
                }
            }
            // A defect without a request id belongs to no single call, so
            // the Effect client fails everything in flight. Mirror that.
            Some("Defect") => {
                let defect = message.get("defect").cloned().unwrap_or(Value::Null);
                for (_, reply) in pending.drain() {
                    reply.fail(RpcError::Defect(defect.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(acks)
}

fn parse_request_id(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn exit_result(exit: Value) -> Result<Value, RpcError> {
    match exit.get("_tag").and_then(Value::as_str) {
        Some("Success") => Ok(exit.get("value").cloned().unwrap_or(Value::Null)),
        _ => {
            let causes = exit
                .get("cause")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let find = |tag: &str, field: &str| {
                causes
                    .iter()
                    .find(|cause| cause.get("_tag").and_then(Value::as_str) == Some(tag))
                    .map(|cause| cause.get(field).cloned().unwrap_or(Value::Null))
            };
            if let Some(error) = find("Fail", "error") {
                Err(RpcError::Failure(error))
            } else if let Some(defect) = find("Die", "defect") {
                Err(RpcError::Defect(defect))
            } else if find("Interrupt", "fiberId").is_some() {
                Err(RpcError::Interrupted)
            } else {
                Err(RpcError::Defect(exit))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_reply() -> (Reply, mpsc::UnboundedReceiver<Result<Value, RpcError>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Reply::Stream(tx), rx)
    }

    #[test]
    fn chunks_are_delivered_in_order_and_acked() {
        let (reply, mut rx) = stream_reply();
        let mut pending = HashMap::from([(3, reply)]);
        let acks = handle_frame(
            r#"{"_tag":"Chunk","requestId":"3","values":[1,2]}"#,
            &mut pending,
        )
        .unwrap();
        assert_eq!(acks, vec![3]);
        assert_eq!(rx.try_recv().unwrap().unwrap(), json!(1));
        assert_eq!(rx.try_recv().unwrap().unwrap(), json!(2));
    }

    #[test]
    fn a_successful_exit_ends_the_stream() {
        let (reply, mut rx) = stream_reply();
        let mut pending = HashMap::from([(1, reply)]);
        handle_frame(
            r#"{"_tag":"Exit","requestId":1,"exit":{"_tag":"Success"}}"#,
            &mut pending,
        )
        .unwrap();
        assert!(pending.is_empty());
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn a_typed_failure_keeps_the_encoded_error() {
        let (tx, mut rx) = oneshot::channel();
        let mut pending = HashMap::from([(0, Reply::Once(tx))]);
        handle_frame(
            r#"[{"_tag":"Exit","requestId":0,"exit":{"_tag":"Failure","cause":[{"_tag":"Fail","error":{"_tag":"NotFound","id":"x"}}]}}]"#,
            &mut pending,
        )
        .unwrap();
        let error = rx.try_recv().unwrap().unwrap_err();
        assert_eq!(error.failure_tag(), Some("NotFound"));
    }

    #[test]
    fn a_connection_defect_fails_every_pending_call() {
        let (tx, mut rx) = oneshot::channel();
        let (reply, mut stream) = stream_reply();
        let mut pending = HashMap::from([(0, Reply::Once(tx)), (1, reply)]);
        handle_frame(r#"{"_tag":"Defect","defect":"boom"}"#, &mut pending).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Err(RpcError::Defect(_))));
        assert!(matches!(
            stream.try_recv().unwrap(),
            Err(RpcError::Defect(_))
        ));
    }
}
