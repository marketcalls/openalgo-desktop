//! The model loop of one turn (agno's run loop): ask the model, stream its
//! prose and reasoning, run the tools it asked for, and ask again until it
//! answers without a tool call, a tool waits for approval, or the turn is
//! stopped.

use super::calls::{BatchOutcome, Calls, PendingCall};
use super::client::{ModelClient, StreamOpenError};
use super::errors::{provider_message, read_genai_error, ProviderFailure};
use super::output::FrameOut;
use super::usage::{Totals, UsageMeter};
use crate::agent::contract::frames::{ErrorKind, Frame};
use crate::agent::contract::model::ResolvedModel;
use futures_util::StreamExt;
use genai::chat::{
    ChatMessage, ChatRequest, ChatStreamEvent, MessageContent, StreamEnd, Tool, ToolCall,
    ToolResponse,
};

/// How a turn's loop ended.
#[derive(Debug)]
pub enum LoopEnd {
    /// The model answered without asking for a tool.
    Completed,
    /// Calls wait for the trader's approval.
    Paused(Vec<PendingCall>),
    /// The run failed; the frame says why.
    Failed(Frame),
    /// A resume whose decisions matched no pending call: it stays paused.
    Unmatched(Vec<PendingCall>),
}

/// What the loop needs besides its frames.
pub struct Turn<'a> {
    pub client: &'a ModelClient,
    pub model: &'a ResolvedModel,
    pub effort: Option<&'static str>,
    pub system: &'a str,
    /// Earlier turns, replayed (`history::replay`).
    pub history: &'a [ChatMessage],
    /// The tools the model is offered, as genai declares them.
    pub declared: Vec<Tool>,
    pub calls: Calls<'a>,
    pub meter: UsageMeter,
    /// This run's own messages, saved with the run.
    pub messages: Vec<ChatMessage>,
}

impl Turn<'_> {
    /// Ask the model until the turn ends.
    pub async fn run(&mut self, out: &mut FrameOut) -> LoopEnd {
        loop {
            let end = match self.ask(out).await {
                Ok(end) => end,
                Err(frame) => return LoopEnd::Failed(frame),
            };
            if let Some(usage) = end.captured_usage.as_ref() {
                if let Some(f) = self.meter.record(Totals::from_genai(usage), self.model) {
                    out.send(f).await;
                }
            }
            let content = end.captured_content.unwrap_or_default();
            let calls: Vec<ToolCall> = content.tool_calls().into_iter().cloned().collect();
            if !content.is_empty() {
                self.messages.push(ChatMessage::assistant(content));
            }
            if calls.is_empty() {
                return LoopEnd::Completed;
            }
            let batch = self.calls.batch(calls, out).await;
            if let Some(end) = self.settle(batch) {
                return end;
            }
        }
    }

    /// Continue a paused turn with the trader's decisions.
    pub async fn resume(
        &mut self,
        pending: Vec<PendingCall>,
        decisions: &std::collections::HashMap<String, bool>,
        note: Option<&str>,
        out: &mut FrameOut,
    ) -> LoopEnd {
        let batch = match self.calls.decide(pending, decisions, note, out).await {
            Ok(b) => b,
            Err(untouched) => return LoopEnd::Unmatched(untouched),
        };
        if let Some(end) = self.settle(batch) {
            return end;
        }
        self.run(out).await
    }

    /// Record a batch's answers; a batch that left calls pending ends the
    /// loop there.
    fn settle(&mut self, batch: BatchOutcome) -> Option<LoopEnd> {
        if !batch.responses.is_empty() {
            // A resumed batch answers the same assistant turn as the calls
            // that ran before the pause: one tool message holds them all,
            // which providers that want every result together require.
            match self.messages.last_mut() {
                Some(last) if last.role == genai::chat::ChatRole::Tool => {
                    for r in batch.responses {
                        last.content.push(r);
                    }
                }
                _ => self
                    .messages
                    .push(ChatMessage::tool(MessageContent::from_tool_responses(
                        batch.responses,
                    ))),
            }
        }
        (!batch.pending.is_empty()).then_some(LoopEnd::Paused(batch.pending))
    }

    /// One model request, streamed: prose and reasoning go out as they
    /// arrive; the captured answer, tool calls and usage come back.
    async fn ask(&mut self, out: &mut FrameOut) -> Result<StreamEnd, Frame> {
        let mut messages = self.history.to_vec();
        messages.extend(self.messages.iter().cloned());
        let mut request = ChatRequest::new(messages).with_system(self.system);
        if !self.declared.is_empty() {
            request = request.with_tools(self.declared.clone());
        }
        let opened = self.client.stream(self.model, request, self.effort).await;
        let mut stream = match opened {
            Ok(r) => r.stream,
            Err(StreamOpenError::Credentials(e)) => {
                return Err(Frame::Error {
                    message: e.message().to_string(),
                    kind: e.kind(),
                })
            }
            Err(StreamOpenError::Provider(e)) => {
                return Err(self.provider_error(&read_genai_error(&e)))
            }
        };
        let mut end = StreamEnd::default();
        while let Some(event) = stream.next().await {
            match event {
                Ok(ChatStreamEvent::Chunk(c)) if !c.content.is_empty() => {
                    self.meter.first_output();
                    out.send(Frame::Token { delta: c.content }).await;
                }
                Ok(ChatStreamEvent::ReasoningChunk(c)) if !c.content.is_empty() => {
                    self.meter.first_output();
                    out.send(Frame::Reasoning { delta: c.content }).await;
                }
                Ok(ChatStreamEvent::End(e)) => end = e,
                Ok(_) => {}
                Err(e) => return Err(self.provider_error(&read_genai_error(&e))),
            }
        }
        Ok(end)
    }

    fn provider_error(&self, failure: &ProviderFailure) -> Frame {
        Frame::Error {
            message: provider_message(failure, &self.model.provider_kind),
            kind: ErrorKind::Provider,
        }
    }
}

/// The tools as genai declares them to the provider.
pub fn declare(tools: &[std::sync::Arc<dyn crate::agent::contract::tool::AgentTool>]) -> Vec<Tool> {
    tools
        .iter()
        .map(|t| {
            let s = t.spec();
            Tool::new(s.name)
                .with_description(s.description.to_string())
                .with_schema(s.parameters.clone())
        })
        .collect()
}

/// The answers to calls that never ran because the run ended first (a
/// stop during a batch), so the stored run stays a valid conversation.
pub fn unanswered(messages: &[ChatMessage]) -> Option<ChatMessage> {
    let asked: Vec<&ToolCall> = messages.last()?.content.tool_calls();
    if asked.is_empty() {
        return None;
    }
    Some(ChatMessage::tool(MessageContent::from_tool_responses(
        asked
            .iter()
            .map(|c| {
                ToolResponse::new(
                    &c.call_id,
                    "Tool call was not executed: the turn was stopped.",
                )
            })
            .collect::<Vec<_>>(),
    )))
}
