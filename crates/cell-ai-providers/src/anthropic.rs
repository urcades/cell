use std::collections::HashMap;
use std::sync::Arc;

use cell_ai_core::{
    AiThinkingLevel, AssistantContentBlock, AssistantMessageEvent, AssistantMessageEventSender,
    AssistantMessageEventStream, Context, Message, Model, StopReason, StreamOptions,
    ToolDefinition, UserContent, UserContentBlock, UserMessage,
};
use cell_oauth::{OAuthCredentials, OAuthProvider, register_oauth_provider};
use reqwest::Client;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};

use crate::ApiProvider;
use crate::common::{
    consume_json_sse, initial_assistant_message, insert_header, post_json, update_usage,
};

const ANTHROPIC_API: &str = "anthropic-messages";
const CLAUDE_CODE_VERSION: &str = "2.1.2";
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

pub(crate) fn register_builtin_oauth_provider() {
    register_oauth_provider(Arc::new(AnthropicOAuthProvider));
}

pub struct AnthropicMessagesProvider;

impl ApiProvider for AnthropicMessagesProvider {
    fn api(&self) -> &'static str {
        ANTHROPIC_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let (mut sender, stream) = AssistantMessageEventStream::new();
        let model = model.clone();
        let context = context.clone();
        let options = options.unwrap_or_default();
        tokio::spawn(async move {
            let mut output = initial_assistant_message(&model);
            let result =
                run_anthropic_stream(&model, &context, &options, &mut sender, &mut output).await;
            match result {
                Ok(()) => sender.send(AssistantMessageEvent::Done {
                    reason: output.stop_reason,
                    message: output,
                }),
                Err(error) => {
                    output.stop_reason = StopReason::Error;
                    output.error_message = Some(error);
                    sender.send(AssistantMessageEvent::Error {
                        reason: StopReason::Error,
                        error: output,
                    });
                }
            }
        });
        stream
    }
}

#[derive(Default)]
struct AnthropicOAuthProvider;

impl OAuthProvider for AnthropicOAuthProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn get_api_key(&self, credentials: &OAuthCredentials) -> Option<String> {
        if credentials.access.trim().is_empty() {
            None
        } else {
            Some(credentials.access.clone())
        }
    }
}

#[derive(Default)]
struct AnthropicStreamState {
    blocks_by_index: HashMap<usize, AnthropicBlockState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnthropicBlockKind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Clone, Debug)]
struct AnthropicBlockState {
    content_index: usize,
    kind: AnthropicBlockKind,
    partial_json: String,
}

async fn run_anthropic_stream(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    sender: &mut AssistantMessageEventSender,
    output: &mut cell_ai_core::AssistantMessage,
) -> Result<(), String> {
    let api_key = options
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("No API key for provider: {}", model.provider.0))?;
    let is_oauth = is_oauth_token(&api_key);
    let url = resolve_anthropic_url(&model.base_url);
    let request = build_anthropic_request(model, context, options, is_oauth);
    let headers = build_anthropic_headers(
        model.headers.as_ref(),
        options.headers.as_ref(),
        &api_key,
        is_oauth,
    );
    let client = Client::new();
    let response = post_json(&client, &url, headers, &request).await?;

    sender.send(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut state = AnthropicStreamState::default();
    consume_json_sse(response, |event| {
        process_anthropic_event(&event, output, &mut state, sender, model)
    })
    .await
}

fn build_anthropic_headers(
    model_headers: Option<&HashMap<String, String>>,
    option_headers: Option<&HashMap<String, String>>,
    api_key: &str,
    is_oauth: bool,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, "accept", "application/json");
    insert_header(&mut headers, "content-type", "application/json");
    insert_header(&mut headers, "anthropic-version", "2023-06-01");
    insert_header(
        &mut headers,
        "anthropic-dangerous-direct-browser-access",
        "true",
    );

    let beta_features = if is_oauth {
        "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14"
    } else {
        "fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14"
    };
    insert_header(&mut headers, "anthropic-beta", beta_features);

    if is_oauth {
        insert_header(&mut headers, "authorization", &format!("Bearer {api_key}"));
        insert_header(
            &mut headers,
            "user-agent",
            &format!("claude-cli/{CLAUDE_CODE_VERSION} (external, cli)"),
        );
        insert_header(&mut headers, "x-app", "cli");
    } else {
        insert_header(&mut headers, "x-api-key", api_key);
    }

    for header_set in [model_headers, option_headers] {
        let Some(header_set) = header_set else {
            continue;
        };
        for (key, value) in header_set {
            insert_header(&mut headers, key, value);
        }
    }

    headers
}

fn build_anthropic_request(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    is_oauth: bool,
) -> Value {
    let mut request = json!({
        "model": model.id,
        "messages": convert_anthropic_messages(model, &context.messages, is_oauth),
        "max_tokens": options.max_tokens.unwrap_or((model.max_tokens / 3).max(1024)),
        "stream": true,
    });

    if let Some(system_prompt) =
        build_anthropic_system_prompt(context.system_prompt.as_deref(), is_oauth)
    {
        request["system"] = system_prompt;
    }
    if let Some(temperature) = options
        .temperature
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok())
    {
        request["temperature"] = json!(temperature);
    }
    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        request["tools"] = Value::Array(convert_anthropic_tools(tools, is_oauth));
        request["tool_choice"] = json!({ "type": "auto" });
    }
    if let Some(thinking_config) = build_anthropic_thinking(model, options.reasoning) {
        if let Some(thinking) = thinking_config.get("thinking") {
            request["thinking"] = thinking.clone();
        }
        if let Some(output_config) = thinking_config.get("output_config") {
            request["output_config"] = output_config.clone();
        }
    }
    if let Some(metadata) = options.metadata.as_ref() {
        if let Some(user_id) = metadata.get("user_id").and_then(Value::as_str) {
            request["metadata"] = json!({ "user_id": user_id });
        }
    }

    request
}

fn process_anthropic_event(
    event: &Value,
    output: &mut cell_ai_core::AssistantMessage,
    state: &mut AnthropicStreamState,
    sender: &mut AssistantMessageEventSender,
    model: &Model,
) -> Result<(), String> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing Anthropic event type".to_string())?;

    match event_type {
        "message_start" => {
            if let Some(usage) = event
                .get("message")
                .and_then(Value::as_object)
                .and_then(|message| message.get("usage"))
                .and_then(Value::as_object)
            {
                let input = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let cache_write = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                output.usage = update_usage(model, input, output_tokens, cache_read, cache_write);
            }
        }
        "content_block_start" => {
            let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let content_block = event
                .get("content_block")
                .and_then(Value::as_object)
                .ok_or_else(|| "Anthropic content_block_start missing content_block".to_string())?;
            match content_block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    output.content.push(AssistantContentBlock::Text {
                        text: String::new(),
                        text_signature: None,
                    });
                    let content_index = output.content.len() - 1;
                    state.blocks_by_index.insert(
                        index,
                        AnthropicBlockState {
                            content_index,
                            kind: AnthropicBlockKind::Text,
                            partial_json: String::new(),
                        },
                    );
                    sender.send(AssistantMessageEvent::TextStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                Some("thinking") => {
                    output.content.push(AssistantContentBlock::Thinking {
                        thinking: String::new(),
                        thinking_signature: Some(String::new()),
                    });
                    let content_index = output.content.len() - 1;
                    state.blocks_by_index.insert(
                        index,
                        AnthropicBlockState {
                            content_index,
                            kind: AnthropicBlockKind::Thinking,
                            partial_json: String::new(),
                        },
                    );
                    sender.send(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                Some("tool_use") => {
                    let id = content_block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = content_block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = content_block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    output.content.push(AssistantContentBlock::ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments,
                        thought_signature: None,
                    });
                    let content_index = output.content.len() - 1;
                    state.blocks_by_index.insert(
                        index,
                        AnthropicBlockState {
                            content_index,
                            kind: AnthropicBlockKind::ToolCall,
                            partial_json: String::new(),
                        },
                    );
                    sender.send(AssistantMessageEvent::ToolcallStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let delta = event
                .get("delta")
                .and_then(Value::as_object)
                .ok_or_else(|| "Anthropic content_block_delta missing delta".to_string())?;
            let Some(block_state) = state.blocks_by_index.get_mut(&index) else {
                return Ok(());
            };

            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") if block_state.kind == AnthropicBlockKind::Text => {
                    let text_delta = delta
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if let Some(AssistantContentBlock::Text { text, .. }) =
                        output.content.get_mut(block_state.content_index)
                    {
                        text.push_str(&text_delta);
                    }
                    sender.send(AssistantMessageEvent::TextDelta {
                        content_index: block_state.content_index,
                        delta: text_delta,
                        partial: output.clone(),
                    });
                }
                Some("thinking_delta") if block_state.kind == AnthropicBlockKind::Thinking => {
                    let thinking_delta = delta
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if let Some(AssistantContentBlock::Thinking { thinking, .. }) =
                        output.content.get_mut(block_state.content_index)
                    {
                        thinking.push_str(&thinking_delta);
                    }
                    sender.send(AssistantMessageEvent::ThinkingDelta {
                        content_index: block_state.content_index,
                        delta: thinking_delta,
                        partial: output.clone(),
                    });
                }
                Some("input_json_delta") if block_state.kind == AnthropicBlockKind::ToolCall => {
                    let json_delta = delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    block_state.partial_json.push_str(&json_delta);
                    if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                        output.content.get_mut(block_state.content_index)
                    {
                        *arguments = serde_json::from_str(&block_state.partial_json)
                            .unwrap_or_else(|_| json!({}));
                    }
                    sender.send(AssistantMessageEvent::ToolcallDelta {
                        content_index: block_state.content_index,
                        delta: json_delta,
                        partial: output.clone(),
                    });
                }
                Some("signature_delta") if block_state.kind == AnthropicBlockKind::Thinking => {
                    if let Some(AssistantContentBlock::Thinking {
                        thinking_signature: Some(signature),
                        ..
                    }) = output.content.get_mut(block_state.content_index)
                    {
                        signature.push_str(
                            delta
                                .get("signature")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let Some(block_state) = state.blocks_by_index.remove(&index) else {
                return Ok(());
            };
            match block_state.kind {
                AnthropicBlockKind::Text => {
                    if let Some(AssistantContentBlock::Text { text, .. }) =
                        output.content.get(block_state.content_index)
                    {
                        sender.send(AssistantMessageEvent::TextEnd {
                            content_index: block_state.content_index,
                            content: text.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                AnthropicBlockKind::Thinking => {
                    if let Some(AssistantContentBlock::Thinking { thinking, .. }) =
                        output.content.get(block_state.content_index)
                    {
                        sender.send(AssistantMessageEvent::ThinkingEnd {
                            content_index: block_state.content_index,
                            content: thinking.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                AnthropicBlockKind::ToolCall => {
                    if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                        output.content.get_mut(block_state.content_index)
                    {
                        *arguments = serde_json::from_str(&block_state.partial_json)
                            .unwrap_or_else(|_| arguments.clone());
                    }
                    if let Some(tool_call) = output.content.get(block_state.content_index).cloned()
                    {
                        sender.send(AssistantMessageEvent::ToolcallEnd {
                            content_index: block_state.content_index,
                            tool_call,
                            partial: output.clone(),
                        });
                    }
                }
            }
        }
        "message_delta" => {
            if let Some(stop_reason) = event
                .get("delta")
                .and_then(Value::as_object)
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
            {
                output.stop_reason = map_anthropic_stop_reason(stop_reason);
            }

            if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                let input = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(output.usage.input);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(output.usage.output);
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(output.usage.cache_read);
                let cache_write = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(output.usage.cache_write);
                output.usage = update_usage(model, input, output_tokens, cache_read, cache_write);
            }
        }
        "error" => {
            let message = event
                .get("error")
                .and_then(Value::as_object)
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Anthropic stream error");
            return Err(message.to_string());
        }
        _ => {}
    }

    Ok(())
}

fn resolve_anthropic_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/v1/messages") {
        normalized.to_string()
    } else if normalized.ends_with("/v1") {
        format!("{normalized}/messages")
    } else {
        format!("{normalized}/v1/messages")
    }
}

fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

fn build_anthropic_system_prompt(system_prompt: Option<&str>, is_oauth: bool) -> Option<Value> {
    let mut blocks = Vec::new();
    if is_oauth {
        blocks.push(json!({
            "type": "text",
            "text": CLAUDE_CODE_IDENTITY,
        }));
    }
    if let Some(system_prompt) = system_prompt {
        blocks.push(json!({
            "type": "text",
            "text": system_prompt,
        }));
    }
    if blocks.is_empty() {
        None
    } else {
        Some(Value::Array(blocks))
    }
}

fn convert_anthropic_messages(model: &Model, messages: &[Message], is_oauth: bool) -> Vec<Value> {
    let mut converted = Vec::new();
    let supports_images = model
        .input
        .iter()
        .any(|input| matches!(input, cell_ai_core::ModelInput::Image));

    let mut index = 0;
    while index < messages.len() {
        match &messages[index] {
            Message::User(UserMessage { content, .. }) => {
                match content {
                    UserContent::Text(text) => {
                        if !text.trim().is_empty() {
                            converted.push(json!({
                                "role": "user",
                                "content": text,
                            }));
                        }
                    }
                    UserContent::Blocks(blocks) => {
                        let blocks = convert_anthropic_user_blocks(blocks, supports_images);
                        if !blocks.is_empty() {
                            converted.push(json!({
                                "role": "user",
                                "content": blocks,
                            }));
                        }
                    }
                }
                index += 1;
            }
            Message::Assistant(assistant) => {
                let mut blocks = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text { text, .. } if !text.trim().is_empty() => blocks.push(json!({
                            "type": "text",
                            "text": text,
                        })),
                        AssistantContentBlock::Thinking {
                            thinking,
                            thinking_signature: Some(signature),
                        } if !thinking.trim().is_empty() && !signature.trim().is_empty() => blocks.push(json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": signature,
                        })),
                        AssistantContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => {
                            blocks.push(json!({
                                "type": "text",
                                "text": thinking,
                            }));
                        }
                        AssistantContentBlock::ToolCall { id, name, arguments, .. } => blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": if is_oauth { to_claude_code_tool_name(name) } else { name.to_string() },
                            "input": arguments,
                        })),
                        _ => {}
                    }
                }
                if !blocks.is_empty() {
                    converted.push(json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
                index += 1;
            }
            Message::ToolResult(_) => {
                let mut tool_results = Vec::new();
                while index < messages.len() {
                    let Message::ToolResult(tool_result) = &messages[index] else {
                        break;
                    };
                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_result.tool_call_id,
                        "content": anthropic_tool_result_content(tool_result, supports_images),
                        "is_error": tool_result.is_error,
                    }));
                    index += 1;
                }
                converted.push(json!({
                    "role": "user",
                    "content": tool_results,
                }));
            }
        }
    }

    converted
}

fn convert_anthropic_user_blocks(blocks: &[UserContentBlock], supports_images: bool) -> Vec<Value> {
    let mut converted = Vec::new();
    for block in blocks {
        match block {
            UserContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                converted.push(json!({
                    "type": "text",
                    "text": text,
                }))
            }
            UserContentBlock::Image { data, mime_type } if supports_images => {
                converted.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": data,
                    }
                }))
            }
            _ => {}
        }
    }
    converted
}

fn anthropic_tool_result_content(
    tool_result: &cell_ai_core::ToolResultMessage,
    supports_images: bool,
) -> Value {
    let mut blocks = Vec::new();
    for block in &tool_result.content {
        match block {
            UserContentBlock::Text { text, .. } if !text.trim().is_empty() => blocks.push(json!({
                "type": "text",
                "text": text,
            })),
            UserContentBlock::Image { data, mime_type } if supports_images => blocks.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime_type,
                    "data": data,
                }
            })),
            _ => {}
        }
    }

    let has_text = blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("text"));
    let has_image = blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) == Some("image"));
    if has_image && !has_text {
        blocks.insert(
            0,
            json!({
                "type": "text",
                "text": "(see attached image)",
            }),
        );
    }

    if blocks.len() == 1 && blocks[0].get("type").and_then(Value::as_str) == Some("text") {
        blocks[0].get("text").cloned().unwrap_or_else(|| json!(""))
    } else {
        Value::Array(blocks)
    }
}

fn convert_anthropic_tools(tools: &[ToolDefinition], is_oauth: bool) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": if is_oauth { to_claude_code_tool_name(&tool.name) } else { tool.name.clone() },
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

fn to_claude_code_tool_name(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "read" => "Read".to_string(),
        "write" => "Write".to_string(),
        "edit" => "Edit".to_string(),
        "bash" => "Bash".to_string(),
        "grep" => "Grep".to_string(),
        "find" => "Glob".to_string(),
        other => other.to_string(),
    }
}

fn build_anthropic_thinking(model: &Model, reasoning: Option<AiThinkingLevel>) -> Option<Value> {
    let reasoning = reasoning?;
    if supports_adaptive_thinking(&model.id) {
        Some(json!({
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": map_anthropic_effort(reasoning) },
        }))
    } else {
        Some(json!({
            "thinking": {
                "type": "enabled",
                "budget_tokens": map_anthropic_budget(reasoning),
            }
        }))
    }
}

fn supports_adaptive_thinking(model_id: &str) -> bool {
    model_id.contains("opus-4-6") || model_id.contains("opus-4.6")
}

fn map_anthropic_effort(level: AiThinkingLevel) -> &'static str {
    match level {
        AiThinkingLevel::Minimal | AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High | AiThinkingLevel::Xhigh => "high",
    }
}

fn map_anthropic_budget(level: AiThinkingLevel) -> u32 {
    match level {
        AiThinkingLevel::Minimal => 1024,
        AiThinkingLevel::Low => 2048,
        AiThinkingLevel::Medium => 4096,
        AiThinkingLevel::High => 8192,
        AiThinkingLevel::Xhigh => 16384,
    }
}

fn map_anthropic_stop_reason(stop_reason: &str) -> StopReason {
    match stop_reason {
        "max_tokens" => StopReason::Length,
        "tool_use" => StopReason::ToolUse,
        "refusal" | "sensitive_content_error" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use cell_ai_core::{
        ApiId, AssistantContentBlock, Context, Message, Model, ModelCost, ModelInput, ProviderId,
        StopReason, StreamOptions, ToolDefinition, ToolResultMessage, UserContent,
        UserContentBlock, UserMessage,
    };
    use serde_json::json;

    use super::{
        AnthropicStreamState, build_anthropic_request, process_anthropic_event,
        resolve_anthropic_url,
    };
    use crate::common::initial_assistant_message;

    fn model() -> Model {
        Model {
            id: "claude-opus-4-6".to_string(),
            name: "Claude Opus 4.6".to_string(),
            api: ApiId::new("anthropic-messages"),
            provider: ProviderId::new("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            input: vec![ModelInput::Text, ModelInput::Image],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 200_000,
            max_tokens: 8192,
            headers: None,
            compat: None,
        }
    }

    fn context() -> Context {
        Context {
            system_prompt: Some("system".to_string()),
            messages: vec![
                Message::User(UserMessage {
                    content: UserContent::Text("hello".to_string()),
                    timestamp: 1,
                }),
                Message::ToolResult(ToolResultMessage {
                    tool_call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "result".to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 2,
                }),
            ],
            tools: Some(vec![ToolDefinition {
                name: "read".to_string(),
                description: "Read".to_string(),
                parameters: json!({"type":"object"}),
            }]),
        }
    }

    #[test]
    fn builds_request_with_grouped_tool_results_and_thinking() {
        let request = build_anthropic_request(
            &model(),
            &context(),
            &StreamOptions {
                reasoning: Some(cell_ai_core::AiThinkingLevel::High),
                ..StreamOptions::default()
            },
            false,
        );

        assert_eq!(request["messages"][0]["role"], json!("user"));
        assert_eq!(
            request["messages"][1]["content"][0]["type"],
            json!("tool_result")
        );
        assert_eq!(request["thinking"]["type"], json!("adaptive"));
        assert_eq!(request["output_config"]["effort"], json!("high"));
    }

    #[test]
    fn maps_anthropic_stream_events() {
        let model = model();
        let mut output = initial_assistant_message(&model);
        let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
        drop(stream);
        let mut state = AnthropicStreamState::default();

        process_anthropic_event(
            &json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("message start");
        process_anthropic_event(
            &json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("block start");
        process_anthropic_event(
            &json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("block delta");
        process_anthropic_event(
            &json!({"type":"content_block_stop","index":0}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("block stop");
        process_anthropic_event(
            &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("message delta");

        match &output.content[0] {
            AssistantContentBlock::Text { text, .. } => assert_eq!(text, "hello"),
            other => panic!("unexpected block: {other:?}"),
        }
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.usage.input, 10);
    }

    #[test]
    fn resolves_anthropic_endpoint_url() {
        assert_eq!(
            resolve_anthropic_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            resolve_anthropic_url("https://proxy.example.com/v1"),
            "https://proxy.example.com/v1/messages"
        );
    }
}
