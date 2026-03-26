use futures::Stream;
use pin_project_lite::pin_project;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::types::{AssistantMessage, AssistantMessageEvent};

#[derive(Debug, Error)]
pub enum EventStreamError {
    #[error("assistant message result was dropped before completion")]
    MissingResult,
}

#[derive(Debug)]
pub struct AssistantMessageEventSender {
    event_tx: mpsc::UnboundedSender<AssistantMessageEvent>,
    result_tx: Option<oneshot::Sender<AssistantMessage>>,
}

pin_project! {
    #[derive(Debug)]
    pub struct AssistantMessageEventStream {
        #[pin]
        receiver: UnboundedReceiverStream<AssistantMessageEvent>,
        result_rx: oneshot::Receiver<AssistantMessage>,
    }
}

impl AssistantMessageEventStream {
    pub fn new() -> (AssistantMessageEventSender, Self) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();
        (
            AssistantMessageEventSender {
                event_tx,
                result_tx: Some(result_tx),
            },
            Self {
                receiver: UnboundedReceiverStream::new(event_rx),
                result_rx,
            },
        )
    }

    pub async fn result(self) -> Result<AssistantMessage, EventStreamError> {
        self.result_rx
            .await
            .map_err(|_| EventStreamError::MissingResult)
    }
}

impl AssistantMessageEventSender {
    pub fn send(&mut self, event: AssistantMessageEvent) {
        self.maybe_complete(&event);
        let _ = self.event_tx.send(event);
    }

    fn maybe_complete(&mut self, event: &AssistantMessageEvent) {
        let Some(result_tx) = self.result_tx.take() else {
            return;
        };
        match event {
            AssistantMessageEvent::Done { message, .. } => {
                let _ = result_tx.send(message.clone());
            }
            AssistantMessageEvent::Error { error, .. } => {
                let _ = result_tx.send(error.clone());
            }
            _ => {
                self.result_tx = Some(result_tx);
            }
        }
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().receiver.poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::AssistantMessageEventStream;
    use crate::types::{
        ApiId, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, ProviderId,
        StopReason, Usage, UsageCost,
    };

    fn sample_message(reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: "hello".to_string(),
                text_signature: None,
            }],
            api: ApiId::from("openai-responses"),
            provider: ProviderId::from("openai"),
            model: "gpt-5.1-codex".to_string(),
            usage: Usage {
                input: 1,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 3,
                cost: UsageCost {
                    input: "0".to_string(),
                    output: "0".to_string(),
                    cache_read: "0".to_string(),
                    cache_write: "0".to_string(),
                    total: "0".to_string(),
                },
            },
            stop_reason: reason,
            error_message: None,
            timestamp: 0,
        }
    }

    #[tokio::test]
    async fn resolves_done_message_as_result() {
        let (mut sender, mut stream) = AssistantMessageEventStream::new();
        let message = sample_message(StopReason::Stop);
        sender.send(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: message.clone(),
        });

        let event = stream.next().await.expect("event");
        match event {
            AssistantMessageEvent::Done {
                message: final_message,
                ..
            } => assert_eq!(final_message, message),
            other => panic!("unexpected event: {other:?}"),
        }

        let result = stream.result().await.expect("result");
        assert_eq!(result, message);
    }

    #[tokio::test]
    async fn resolves_error_message_as_result() {
        let (mut sender, stream) = AssistantMessageEventStream::new();
        let message = sample_message(StopReason::Error);
        sender.send(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: message.clone(),
        });

        let result = stream.result().await.expect("result");
        assert_eq!(result, message);
    }
}
