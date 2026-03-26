use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cell_ai_core::{
    AiThinkingLevel, AssistantContentBlock, AssistantMessage, AssistantMessageEvent,
    AssistantMessageEventSender, AssistantMessageEventStream, Context, Message, Model, StopReason,
    StreamOptions, ToolDefinition, ToolResultMessage, UserContent, UserContentBlock, UserMessage,
};
use cell_oauth::{OAuthCredentials, OAuthProvider, register_oauth_provider};
use reqwest::Client;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};

use crate::ApiProvider;
use crate::common::{
    consume_json_sse, initial_assistant_message, insert_header, post_json, update_usage,
};

const OPENAI_RESPONSES_API: &str = "openai-responses";
const OPENAI_CODEX_RESPONSES_API: &str = "openai-codex-responses";
const OPENAI_COMPLETIONS_API: &str = "openai-completions";
const CODEX_IDENTITY_CLAIM: &str = "https://api.openai.com/auth";

pub(crate) fn register_builtin_oauth_provider() {
    register_oauth_provider(Arc::new(OpenAICodexOAuthProvider));
}

pub struct OpenAIResponsesProvider;

impl ApiProvider for OpenAIResponsesProvider {
    fn api(&self) -> &'static str {
        OPENAI_RESPONSES_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_openai_responses(model.clone(), context.clone(), options.unwrap_or_default())
    }
}

pub struct OpenAICodexResponsesProvider;

impl ApiProvider for OpenAICodexResponsesProvider {
    fn api(&self) -> &'static str {
        OPENAI_CODEX_RESPONSES_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_openai_codex_responses(model.clone(), context.clone(), options.unwrap_or_default())
    }
}

pub struct OpenAICompletionsProvider;

impl ApiProvider for OpenAICompletionsProvider {
    fn api(&self) -> &'static str {
        OPENAI_COMPLETIONS_API
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_openai_completions(model.clone(), context.clone(), options.unwrap_or_default())
    }
}

#[derive(Default)]
struct OpenAICodexOAuthProvider;

impl OAuthProvider for OpenAICodexOAuthProvider {
    fn id(&self) -> &'static str {
        "openai-codex"
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
struct ResponsesStreamState {
    current_kind: Option<ResponsesItemKind>,
    current_index: Option<usize>,
    tool_json: HashMap<usize, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponsesItemKind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Default)]
struct ChatCompletionsState {
    current_text: Option<usize>,
    current_thinking: Option<usize>,
    tool_calls: HashMap<usize, ChatToolCallState>,
}

#[derive(Clone, Debug)]
struct ChatToolCallState {
    content_index: usize,
    partial_json: String,
    closed: bool,
}

fn stream_openai_responses(
    model: Model,
    context: Context,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    let (mut sender, stream) = AssistantMessageEventStream::new();
    tokio::spawn(async move {
        let mut output = initial_assistant_message(&model);
        let result =
            run_openai_responses(&model, &context, &options, &mut sender, &mut output).await;
        finalize_stream_result(&mut sender, &mut output, result);
    });
    stream
}

fn stream_openai_codex_responses(
    model: Model,
    context: Context,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    let (mut sender, stream) = AssistantMessageEventStream::new();
    tokio::spawn(async move {
        let mut output = initial_assistant_message(&model);
        let result =
            run_openai_codex_responses(&model, &context, &options, &mut sender, &mut output).await;
        finalize_stream_result(&mut sender, &mut output, result);
    });
    stream
}

fn stream_openai_completions(
    model: Model,
    context: Context,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    let (mut sender, stream) = AssistantMessageEventStream::new();
    tokio::spawn(async move {
        let mut output = initial_assistant_message(&model);
        let result =
            run_openai_completions(&model, &context, &options, &mut sender, &mut output).await;
        finalize_stream_result(&mut sender, &mut output, result);
    });
    stream
}

async fn run_openai_responses(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    sender: &mut AssistantMessageEventSender,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    let api_key = require_api_key(options, model)?;
    let url = resolve_responses_url(&model.base_url);
    let request = build_openai_responses_request(model, context, options);
    let headers = build_openai_headers(
        model.headers.as_ref(),
        options.headers.as_ref(),
        &api_key,
        false,
    );
    let client = Client::new();
    let response = post_json(&client, &url, headers, &request).await?;

    sender.send(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut state = ResponsesStreamState::default();
    consume_json_sse(response, |event| {
        process_openai_responses_event(&event, output, &mut state, sender, model)
    })
    .await
}

async fn run_openai_codex_responses(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    sender: &mut AssistantMessageEventSender,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    let api_key = require_api_key(options, model)?;
    let url = resolve_codex_url(&model.base_url);
    let request = build_openai_codex_request(model, context, options);
    let headers = build_openai_codex_headers(
        model.headers.as_ref(),
        options.headers.as_ref(),
        &api_key,
        options,
    );
    let client = Client::new();
    let response = post_json(&client, &url, headers, &request).await?;

    sender.send(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut state = ResponsesStreamState::default();
    consume_json_sse(response, |event| {
        let normalized = normalize_codex_event(event);
        process_openai_responses_event(&normalized, output, &mut state, sender, model)
    })
    .await
}

async fn run_openai_completions(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    sender: &mut AssistantMessageEventSender,
    output: &mut AssistantMessage,
) -> Result<(), String> {
    let api_key = require_api_key(options, model)?;
    let url = resolve_chat_completions_url(&model.base_url);
    let request = build_chat_completions_request(model, context, options);
    let mut headers = build_openai_headers(
        model.headers.as_ref(),
        options.headers.as_ref(),
        &api_key,
        false,
    );
    if model.provider.0 == "openrouter" {
        insert_header(&mut headers, "HTTP-Referer", env!("CARGO_PKG_REPOSITORY"));
        insert_header(&mut headers, "X-Title", "cell");
    }

    let client = Client::new();
    let response = post_json(&client, &url, headers, &request).await?;

    sender.send(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut state = ChatCompletionsState::default();
    consume_json_sse(response, |event| {
        process_chat_completions_event(&event, output, &mut state, sender, model)
    })
    .await?;
    finalize_chat_completions_blocks(output, &mut state, sender)?;
    if output.stop_reason == StopReason::Stop && has_tool_calls(output) {
        output.stop_reason = StopReason::ToolUse;
    }
    Ok(())
}

fn finalize_stream_result(
    sender: &mut AssistantMessageEventSender,
    output: &mut AssistantMessage,
    result: Result<(), String>,
) {
    match result {
        Ok(()) => sender.send(AssistantMessageEvent::Done {
            reason: output.stop_reason,
            message: output.clone(),
        }),
        Err(error) => {
            output.stop_reason = StopReason::Error;
            output.error_message = Some(error);
            sender.send(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: output.clone(),
            });
        }
    }
}

fn require_api_key(options: &StreamOptions, model: &Model) -> Result<String, String> {
    options
        .api_key
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("No API key for provider: {}", model.provider.0))
}

fn build_openai_headers(
    model_headers: Option<&HashMap<String, String>>,
    option_headers: Option<&HashMap<String, String>>,
    api_key: &str,
    experimental_responses: bool,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, "accept", "text/event-stream");
    insert_header(&mut headers, "content-type", "application/json");
    insert_header(&mut headers, "authorization", &format!("Bearer {api_key}"));
    if experimental_responses {
        insert_header(&mut headers, "OpenAI-Beta", "responses=experimental");
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

fn build_openai_codex_headers(
    model_headers: Option<&HashMap<String, String>>,
    option_headers: Option<&HashMap<String, String>>,
    api_key: &str,
    options: &StreamOptions,
) -> HeaderMap {
    let mut headers = build_openai_headers(model_headers, option_headers, api_key, true);
    insert_header(&mut headers, "originator", "pi");
    insert_header(
        &mut headers,
        "user-agent",
        &format!("cell/{}", env!("CARGO_PKG_VERSION")),
    );
    if let Some(account_id) = extract_chatgpt_account_id(api_key) {
        insert_header(&mut headers, "chatgpt-account-id", &account_id);
    }
    if let Some(session_id) = &options.session_id {
        insert_header(&mut headers, "session_id", session_id);
    }
    headers
}

fn build_openai_responses_request(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Value {
    let mut request = json!({
        "model": model.id,
        "input": convert_responses_input(model, context, true),
        "stream": true,
        "store": false,
    });

    if let Some(max_tokens) = options.max_tokens {
        request["max_output_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = parse_temperature(&options.temperature) {
        request["temperature"] = json!(temperature);
    }
    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        request["tools"] = Value::Array(convert_responses_tools(tools));
    }
    if let Some(reasoning) = build_openai_reasoning(options.reasoning) {
        request["reasoning"] = reasoning;
        request["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let Some(session_id) = &options.session_id {
        request["prompt_cache_key"] = json!(session_id);
    }

    request
}

fn build_openai_codex_request(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    let mut request = json!({
        "model": model.id,
        "store": false,
        "stream": true,
        "instructions": context.system_prompt,
        "input": convert_responses_input(model, context, false),
        "text": { "verbosity": "medium" },
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
    });

    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        request["tools"] = Value::Array(convert_responses_tools(tools));
    }
    if let Some(temperature) = parse_temperature(&options.temperature) {
        request["temperature"] = json!(temperature);
    }
    if let Some(session_id) = &options.session_id {
        request["prompt_cache_key"] = json!(session_id);
    }
    if let Some(reasoning) = build_codex_reasoning(options.reasoning) {
        request["reasoning"] = reasoning;
    }

    request
}

fn build_chat_completions_request(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Value {
    let mut request = json!({
        "model": model.id,
        "messages": convert_chat_messages(model, context),
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    if let Some(max_tokens) = options.max_tokens {
        request["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = parse_temperature(&options.temperature) {
        request["temperature"] = json!(temperature);
    }
    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        request["tools"] = Value::Array(convert_chat_tools(tools));
        request["tool_choice"] = json!("auto");
    }
    if let Some(reasoning) = build_chat_reasoning(options.reasoning) {
        request["reasoning_effort"] = reasoning;
    }

    request
}

fn convert_responses_input(
    model: &Model,
    context: &Context,
    include_system_prompt: bool,
) -> Vec<Value> {
    let mut messages = Vec::new();
    let supports_images = model
        .input
        .iter()
        .any(|input| matches!(input, cell_ai_core::ModelInput::Image));

    if include_system_prompt {
        if let Some(system_prompt) = &context.system_prompt {
            let role = if model.reasoning {
                "developer"
            } else {
                "system"
            };
            messages.push(json!({
                "role": role,
                "content": system_prompt,
            }));
        }
    }

    for message in &context.messages {
        match message {
            Message::User(UserMessage { content, .. }) => {
                let Some(content) = convert_responses_user_content(content, supports_images) else {
                    continue;
                };
                messages.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text {
                            text,
                            text_signature,
                        } => {
                            if text.is_empty() {
                                continue;
                            }
                            messages.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                                "status": "completed",
                                "id": text_signature.clone().unwrap_or_else(|| format!("msg_{}", messages.len())),
                            }));
                        }
                        AssistantContentBlock::Thinking {
                            thinking_signature: Some(signature),
                            ..
                        } => {
                            if let Ok(item) = serde_json::from_str::<Value>(signature) {
                                messages.push(item);
                            }
                        }
                        AssistantContentBlock::Thinking { .. } => {}
                        AssistantContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => {
                            let (call_id, item_id, _) =
                                normalize_response_tool_call_id(id, messages.len());
                            messages.push(json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "id": item_id,
                                "name": name,
                                "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                            }));
                        }
                    }
                }
            }
            Message::ToolResult(tool_result) => {
                let text_output = tool_result_text(tool_result);
                let (call_id, _, _) =
                    normalize_response_tool_call_id(&tool_result.tool_call_id, messages.len());
                messages.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": if text_output.is_empty() { "(see attached image)" } else { &text_output },
                }));

                let images = collect_tool_result_images(tool_result);
                if supports_images && !images.is_empty() {
                    let mut content = vec![json!({
                        "type": "input_text",
                        "text": "Attached image(s) from tool result:",
                    })];
                    for image in images {
                        content.push(json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": format!("data:{};base64,{}", image.0, image.1),
                        }));
                    }
                    messages.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
        }
    }

    messages
}

fn convert_chat_messages(model: &Model, context: &Context) -> Vec<Value> {
    let mut messages = Vec::new();
    let supports_images = model
        .input
        .iter()
        .any(|input| matches!(input, cell_ai_core::ModelInput::Image));

    if let Some(system_prompt) = &context.system_prompt {
        messages.push(json!({
            "role": "system",
            "content": system_prompt,
        }));
    }

    for message in &context.messages {
        match message {
            Message::User(UserMessage { content, .. }) => match content {
                UserContent::Text(text) => {
                    if !text.is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": text,
                        }));
                    }
                }
                UserContent::Blocks(blocks) => {
                    let content = convert_chat_user_content(blocks, supports_images);
                    if content.is_null() {
                        continue;
                    }
                    messages.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            },
            Message::Assistant(assistant) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text { text, .. } => {
                            if !text.is_empty() {
                                text_parts.push(text.clone());
                            }
                        }
                        AssistantContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => {
                            tool_calls.push(json!({
                                "id": normalize_chat_tool_call_id(id),
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                                }
                            }));
                        }
                        AssistantContentBlock::Thinking { .. } => {}
                    }
                }

                if text_parts.is_empty() && tool_calls.is_empty() {
                    continue;
                }

                let mut message = json!({
                    "role": "assistant",
                    "content": if text_parts.is_empty() {
                        Value::Null
                    } else {
                        Value::String(text_parts.join("\n"))
                    },
                });
                if !tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(message);
            }
            Message::ToolResult(tool_result) => {
                let text_output = tool_result_text(tool_result);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": normalize_chat_tool_call_id(&tool_result.tool_call_id),
                    "content": if text_output.is_empty() { "(see attached image)" } else { &text_output },
                }));
            }
        }
    }

    messages
}

fn convert_responses_user_content(
    content: &UserContent,
    supports_images: bool,
) -> Option<Vec<Value>> {
    match content {
        UserContent::Text(text) => {
            if text.is_empty() {
                None
            } else {
                Some(vec![json!({
                    "type": "input_text",
                    "text": text,
                })])
            }
        }
        UserContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    UserContentBlock::Text { text, .. } if !text.is_empty() => parts.push(json!({
                        "type": "input_text",
                        "text": text,
                    })),
                    UserContentBlock::Image { data, mime_type } if supports_images => {
                        parts.push(json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": format!("data:{mime_type};base64,{data}"),
                        }))
                    }
                    _ => {}
                }
            }
            if parts.is_empty() { None } else { Some(parts) }
        }
    }
}

fn convert_chat_user_content(blocks: &[UserContentBlock], supports_images: bool) -> Value {
    let mut text_only = Vec::new();
    let mut parts = Vec::new();

    for block in blocks {
        match block {
            UserContentBlock::Text { text, .. } if !text.is_empty() => {
                text_only.push(text.clone());
                parts.push(json!({ "type": "text", "text": text }));
            }
            UserContentBlock::Image { data, mime_type } if supports_images => {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{mime_type};base64,{data}") },
                }));
            }
            _ => {}
        }
    }

    if supports_images {
        if parts.is_empty() {
            Value::Null
        } else {
            Value::Array(parts)
        }
    } else if text_only.is_empty() {
        Value::Null
    } else {
        Value::String(text_only.join("\n"))
    }
}

fn convert_responses_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

fn convert_chat_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }
            })
        })
        .collect()
}

fn process_openai_responses_event(
    event: &Value,
    output: &mut AssistantMessage,
    state: &mut ResponsesStreamState,
    sender: &mut AssistantMessageEventSender,
    model: &Model,
) -> Result<(), String> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing OpenAI responses event type".to_string())?;

    match event_type {
        "response.output_item.added" => {
            let item = event
                .get("item")
                .and_then(Value::as_object)
                .ok_or_else(|| "response.output_item.added missing item".to_string())?;
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    output.content.push(AssistantContentBlock::Thinking {
                        thinking: String::new(),
                        thinking_signature: None,
                    });
                    let content_index = output.content.len() - 1;
                    state.current_kind = Some(ResponsesItemKind::Thinking);
                    state.current_index = Some(content_index);
                    sender.send(AssistantMessageEvent::ThinkingStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                Some("message") => {
                    output.content.push(AssistantContentBlock::Text {
                        text: String::new(),
                        text_signature: None,
                    });
                    let content_index = output.content.len() - 1;
                    state.current_kind = Some(ResponsesItemKind::Text);
                    state.current_index = Some(content_index);
                    sender.send(AssistantMessageEvent::TextStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                Some("function_call") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                    let raw_id = if item_id.is_empty() {
                        call_id.to_string()
                    } else {
                        format!("{call_id}|{item_id}")
                    };
                    let (_, _, normalized_id) =
                        normalize_response_tool_call_id(&raw_id, output.content.len());
                    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                    let partial_json = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = parse_json_or_empty(&partial_json);
                    output.content.push(AssistantContentBlock::ToolCall {
                        id: normalized_id,
                        name: name.to_string(),
                        arguments,
                        thought_signature: None,
                    });
                    let content_index = output.content.len() - 1;
                    state.current_kind = Some(ResponsesItemKind::ToolCall);
                    state.current_index = Some(content_index);
                    state.tool_json.insert(content_index, partial_json);
                    sender.send(AssistantMessageEvent::ToolcallStart {
                        content_index,
                        partial: output.clone(),
                    });
                }
                _ => {}
            }
        }
        "response.reasoning_summary_text.delta" => {
            let content_index = require_current_index(state, ResponsesItemKind::Thinking)?;
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(AssistantContentBlock::Thinking { thinking, .. }) =
                output.content.get_mut(content_index)
            {
                thinking.push_str(&delta);
            }
            sender.send(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                partial: output.clone(),
            });
        }
        "response.reasoning_summary_part.done" => {
            let content_index = require_current_index(state, ResponsesItemKind::Thinking)?;
            if let Some(AssistantContentBlock::Thinking { thinking, .. }) =
                output.content.get_mut(content_index)
            {
                thinking.push_str("\n\n");
            }
            sender.send(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta: "\n\n".to_string(),
                partial: output.clone(),
            });
        }
        "response.output_text.delta" | "response.refusal.delta" => {
            let content_index = require_current_index(state, ResponsesItemKind::Text)?;
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(AssistantContentBlock::Text { text, .. }) =
                output.content.get_mut(content_index)
            {
                text.push_str(&delta);
            }
            sender.send(AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                partial: output.clone(),
            });
        }
        "response.function_call_arguments.delta" => {
            let content_index = require_current_index(state, ResponsesItemKind::ToolCall)?;
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let partial_json = state.tool_json.entry(content_index).or_default();
            partial_json.push_str(&delta);
            if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                output.content.get_mut(content_index)
            {
                *arguments = parse_json_or_empty(partial_json);
            }
            sender.send(AssistantMessageEvent::ToolcallDelta {
                content_index,
                delta,
                partial: output.clone(),
            });
        }
        "response.function_call_arguments.done" => {
            let content_index = require_current_index(state, ResponsesItemKind::ToolCall)?;
            let final_json = event
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            state.tool_json.insert(content_index, final_json.clone());
            if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                output.content.get_mut(content_index)
            {
                *arguments = parse_json_or_empty(&final_json);
            }
        }
        "response.output_item.done" => {
            let item = event
                .get("item")
                .and_then(Value::as_object)
                .ok_or_else(|| "response.output_item.done missing item".to_string())?;
            let Some(content_index) = state.current_index else {
                return Ok(());
            };
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    if let Some(AssistantContentBlock::Thinking {
                        thinking,
                        thinking_signature,
                    }) = output.content.get_mut(content_index)
                    {
                        *thinking_signature = Some(Value::Object(item.clone()).to_string());
                        sender.send(AssistantMessageEvent::ThinkingEnd {
                            content_index,
                            content: thinking.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                Some("message") => {
                    if let Some(AssistantContentBlock::Text {
                        text,
                        text_signature,
                    }) = output.content.get_mut(content_index)
                    {
                        *text_signature = item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                        sender.send(AssistantMessageEvent::TextEnd {
                            content_index,
                            content: text.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                Some("function_call") => {
                    if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                        output.content.get_mut(content_index)
                    {
                        let final_json = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                            .or_else(|| state.tool_json.remove(&content_index))
                            .unwrap_or_else(|| "{}".to_string());
                        *arguments = parse_json_or_empty(&final_json);
                    }
                    if let Some(tool_call) = output.content.get(content_index).cloned() {
                        sender.send(AssistantMessageEvent::ToolcallEnd {
                            content_index,
                            tool_call,
                            partial: output.clone(),
                        });
                    }
                }
                _ => {}
            }
            state.current_index = None;
            state.current_kind = None;
        }
        "response.completed" => {
            let response = event
                .get("response")
                .and_then(Value::as_object)
                .ok_or_else(|| "response.completed missing response".to_string())?;
            if let Some(usage) = response.get("usage").and_then(Value::as_object) {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let cache_read = usage
                    .get("input_tokens_details")
                    .and_then(Value::as_object)
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                output.usage = update_usage(
                    model,
                    input_tokens.saturating_sub(cache_read),
                    output_tokens,
                    cache_read,
                    0,
                );
            }
            output.stop_reason =
                map_response_status(response.get("status").and_then(Value::as_str));
            if output.stop_reason == StopReason::Stop && has_tool_calls(output) {
                output.stop_reason = StopReason::ToolUse;
            }
        }
        "response.failed" => {
            let message = event
                .get("response")
                .and_then(Value::as_object)
                .and_then(|response| response.get("error"))
                .and_then(Value::as_object)
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("OpenAI response failed");
            return Err(message.to_string());
        }
        "error" => {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("OpenAI stream error");
            return Err(message.to_string());
        }
        _ => {}
    }

    Ok(())
}

fn process_chat_completions_event(
    event: &Value,
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
    model: &Model,
) -> Result<(), String> {
    if let Some(usage) = event.get("usage").and_then(Value::as_object) {
        let input_tokens = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_read = usage
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning_tokens = usage
            .get("completion_tokens_details")
            .and_then(Value::as_object)
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        output.usage = update_usage(
            model,
            input_tokens.saturating_sub(cache_read),
            completion_tokens + reasoning_tokens,
            cache_read,
            0,
        );
    }

    let Some(choice) = event
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)
    else {
        return Ok(());
    };

    if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
        output.stop_reason = map_chat_finish_reason(finish_reason);
    }

    let Some(delta) = choice.get("delta").and_then(Value::as_object) else {
        return Ok(());
    };

    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            ensure_chat_text_block(output, state, sender);
            let content_index = state.current_text.expect("text index");
            if let Some(AssistantContentBlock::Text { text, .. }) =
                output.content.get_mut(content_index)
            {
                text.push_str(content);
            }
            sender.send(AssistantMessageEvent::TextDelta {
                content_index,
                delta: content.to_string(),
                partial: output.clone(),
            });
        }
    }

    if let Some(reasoning_delta) = first_reasoning_delta(delta) {
        ensure_chat_thinking_block(output, state, sender);
        let content_index = state.current_thinking.expect("thinking index");
        if let Some(AssistantContentBlock::Thinking {
            thinking,
            thinking_signature,
        }) = output.content.get_mut(content_index)
        {
            thinking.push_str(&reasoning_delta);
            if thinking_signature.is_none() {
                *thinking_signature = Some("openai-chat-reasoning".to_string());
            }
        }
        sender.send(AssistantMessageEvent::ThinkingDelta {
            content_index,
            delta: reasoning_delta,
            partial: output.clone(),
        });
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        finish_chat_text_block(output, state, sender);
        finish_chat_thinking_block(output, state, sender);

        for tool_call in tool_calls {
            let Some(tool_call) = tool_call.as_object() else {
                continue;
            };
            let tool_index = tool_call
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(state.tool_calls.len() as u64) as usize;
            let tool_state = state.tool_calls.entry(tool_index).or_insert_with(|| {
                let id = tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .map(normalize_chat_tool_call_id)
                    .unwrap_or_else(|| format!("call_{tool_index}"));
                output.content.push(AssistantContentBlock::ToolCall {
                    id,
                    name: String::new(),
                    arguments: json!({}),
                    thought_signature: None,
                });
                let content_index = output.content.len() - 1;
                sender.send(AssistantMessageEvent::ToolcallStart {
                    content_index,
                    partial: output.clone(),
                });
                ChatToolCallState {
                    content_index,
                    partial_json: String::new(),
                    closed: false,
                }
            });

            if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                if let Some(AssistantContentBlock::ToolCall { id: current_id, .. }) =
                    output.content.get_mut(tool_state.content_index)
                {
                    *current_id = normalize_chat_tool_call_id(id);
                }
            }
            if let Some(function) = tool_call.get("function").and_then(Value::as_object) {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    if let Some(AssistantContentBlock::ToolCall {
                        name: current_name, ..
                    }) = output.content.get_mut(tool_state.content_index)
                    {
                        *current_name = name.to_string();
                    }
                }
                if let Some(arguments_delta) = function.get("arguments").and_then(Value::as_str) {
                    tool_state.partial_json.push_str(arguments_delta);
                    if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
                        output.content.get_mut(tool_state.content_index)
                    {
                        *arguments = parse_json_or_empty(&tool_state.partial_json);
                    }
                    sender.send(AssistantMessageEvent::ToolcallDelta {
                        content_index: tool_state.content_index,
                        delta: arguments_delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
        }
    }

    Ok(())
}

fn finalize_chat_completions_blocks(
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
) -> Result<(), String> {
    finish_chat_text_block(output, state, sender);
    finish_chat_thinking_block(output, state, sender);

    let mut indices = state.tool_calls.keys().copied().collect::<Vec<_>>();
    indices.sort_unstable();
    for tool_index in indices {
        let Some(tool_state) = state.tool_calls.get_mut(&tool_index) else {
            continue;
        };
        if tool_state.closed {
            continue;
        }
        if let Some(AssistantContentBlock::ToolCall { arguments, .. }) =
            output.content.get_mut(tool_state.content_index)
        {
            *arguments = parse_json_or_empty(&tool_state.partial_json);
        }
        let Some(tool_call) = output.content.get(tool_state.content_index).cloned() else {
            return Err("missing tool call block".to_string());
        };
        sender.send(AssistantMessageEvent::ToolcallEnd {
            content_index: tool_state.content_index,
            tool_call,
            partial: output.clone(),
        });
        tool_state.closed = true;
    }

    Ok(())
}

fn ensure_chat_text_block(
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
) {
    if state.current_text.is_some() {
        return;
    }
    finish_chat_thinking_block(output, state, sender);
    output.content.push(AssistantContentBlock::Text {
        text: String::new(),
        text_signature: None,
    });
    let content_index = output.content.len() - 1;
    state.current_text = Some(content_index);
    sender.send(AssistantMessageEvent::TextStart {
        content_index,
        partial: output.clone(),
    });
}

fn finish_chat_text_block(
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
) {
    let Some(content_index) = state.current_text.take() else {
        return;
    };
    let Some(AssistantContentBlock::Text { text, .. }) = output.content.get(content_index) else {
        return;
    };
    sender.send(AssistantMessageEvent::TextEnd {
        content_index,
        content: text.clone(),
        partial: output.clone(),
    });
}

fn ensure_chat_thinking_block(
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
) {
    if state.current_thinking.is_some() {
        return;
    }
    finish_chat_text_block(output, state, sender);
    output.content.push(AssistantContentBlock::Thinking {
        thinking: String::new(),
        thinking_signature: None,
    });
    let content_index = output.content.len() - 1;
    state.current_thinking = Some(content_index);
    sender.send(AssistantMessageEvent::ThinkingStart {
        content_index,
        partial: output.clone(),
    });
}

fn finish_chat_thinking_block(
    output: &mut AssistantMessage,
    state: &mut ChatCompletionsState,
    sender: &mut AssistantMessageEventSender,
) {
    let Some(content_index) = state.current_thinking.take() else {
        return;
    };
    let Some(AssistantContentBlock::Thinking { thinking, .. }) = output.content.get(content_index)
    else {
        return;
    };
    sender.send(AssistantMessageEvent::ThinkingEnd {
        content_index,
        content: thinking.clone(),
        partial: output.clone(),
    });
}

fn require_current_index(
    state: &ResponsesStreamState,
    expected_kind: ResponsesItemKind,
) -> Result<usize, String> {
    match (state.current_kind, state.current_index) {
        (Some(kind), Some(index)) if kind == expected_kind => Ok(index),
        _ => Err(format!("unexpected stream state for {expected_kind:?}")),
    }
}

fn build_openai_reasoning(reasoning: Option<AiThinkingLevel>) -> Option<Value> {
    let effort = reasoning.map(map_openai_reasoning_effort)?;
    Some(json!({
        "effort": effort,
        "summary": "auto",
    }))
}

fn build_codex_reasoning(reasoning: Option<AiThinkingLevel>) -> Option<Value> {
    let effort = reasoning.map(map_codex_reasoning_effort)?;
    Some(json!({
        "effort": effort,
        "summary": "auto",
    }))
}

fn build_chat_reasoning(reasoning: Option<AiThinkingLevel>) -> Option<Value> {
    reasoning.map(|level| json!(map_openai_reasoning_effort(level)))
}

fn map_openai_reasoning_effort(level: AiThinkingLevel) -> &'static str {
    match level {
        AiThinkingLevel::Minimal => "minimal",
        AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High | AiThinkingLevel::Xhigh => "high",
    }
}

fn map_codex_reasoning_effort(level: AiThinkingLevel) -> &'static str {
    match level {
        AiThinkingLevel::Minimal => "minimal",
        AiThinkingLevel::Low => "low",
        AiThinkingLevel::Medium => "medium",
        AiThinkingLevel::High | AiThinkingLevel::Xhigh => "high",
    }
}

fn map_response_status(status: Option<&str>) -> StopReason {
    match status {
        Some("incomplete") => StopReason::Length,
        Some("failed") | Some("cancelled") => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn map_chat_finish_reason(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::Length,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

fn has_tool_calls(message: &AssistantMessage) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall { .. }))
}

fn parse_temperature(value: &Option<String>) -> Option<f64> {
    value.as_deref().and_then(|value| value.parse::<f64>().ok())
}

fn parse_json_or_empty(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| json!({}))
}

fn tool_result_text(tool_result: &ToolResultMessage) -> String {
    tool_result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text { text, .. } => Some(text.as_str()),
            UserContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_tool_result_images(tool_result: &ToolResultMessage) -> Vec<(String, String)> {
    tool_result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Image { data, mime_type } => Some((mime_type.clone(), data.clone())),
            UserContentBlock::Text { .. } => None,
        })
        .collect()
}

fn normalize_response_tool_call_id(
    raw_id: &str,
    fallback_index: usize,
) -> (String, String, String) {
    let (call_id, item_id) = raw_id
        .split_once('|')
        .map(|(call_id, item_id)| (call_id.to_string(), item_id.to_string()))
        .unwrap_or_else(|| {
            let fallback = if raw_id.is_empty() {
                format!("call_{fallback_index}")
            } else {
                raw_id.to_string()
            };
            (fallback.clone(), format!("fc_{fallback}"))
        });
    let call_id = sanitize_identifier(&call_id, "call");
    let mut item_id = sanitize_identifier(&item_id, "fc");
    if !item_id.starts_with("fc") {
        item_id = format!("fc_{item_id}");
    }
    let combined = format!("{call_id}|{item_id}");
    (call_id, item_id, combined)
}

fn normalize_chat_tool_call_id(raw_id: &str) -> String {
    sanitize_identifier(raw_id, "call")
}

fn sanitize_identifier(raw: &str, prefix: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let sanitized = if sanitized.is_empty() {
        prefix.to_string()
    } else {
        sanitized
    };
    sanitized.chars().take(64).collect()
}

fn resolve_responses_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/responses") {
        normalized.to_string()
    } else {
        format!("{normalized}/responses")
    }
}

fn resolve_chat_completions_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/chat/completions") {
        normalized.to_string()
    } else if normalized.ends_with("/chat") {
        format!("{normalized}/completions")
    } else {
        format!("{normalized}/chat/completions")
    }
}

fn resolve_codex_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn normalize_codex_event(mut event: Value) -> Value {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return event;
    };
    if !matches!(event_type, "response.done" | "response.completed") {
        return event;
    }
    if let Some(object) = event.as_object_mut() {
        object.insert(
            "type".to_string(),
            Value::String("response.completed".to_string()),
        );
    }
    event
}

fn extract_chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get(CODEX_IDENTITY_CLAIM)
        .and_then(Value::as_object)
        .and_then(|claims| claims.get("account_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn first_reasoning_delta(delta: &serde_json::Map<String, Value>) -> Option<String> {
    for field in ["reasoning_content", "reasoning", "reasoning_text"] {
        if let Some(value) = delta.get(field).and_then(Value::as_str) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use cell_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, Context, Model, ModelCost, ModelInput,
        ProviderId, StopReason, StreamOptions, ToolDefinition, Usage, UsageCost, UserContent,
        UserContentBlock, UserMessage,
    };
    use serde_json::json;

    use super::{
        ChatCompletionsState, ResponsesStreamState, build_chat_completions_request,
        build_openai_codex_request, build_openai_responses_request, extract_chatgpt_account_id,
        normalize_codex_event, process_chat_completions_event, process_openai_responses_event,
        resolve_chat_completions_url, resolve_codex_url, resolve_responses_url,
    };
    use crate::common::initial_assistant_message;

    fn model(api: &str, provider: &str, base_url: &str) -> Model {
        Model {
            id: "model-1".to_string(),
            name: "Model 1".to_string(),
            api: ApiId::new(api),
            provider: ProviderId::new(provider),
            base_url: base_url.to_string(),
            reasoning: true,
            input: vec![ModelInput::Text, ModelInput::Image],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 128_000,
            max_tokens: 16_384,
            headers: None,
            compat: None,
        }
    }

    fn context() -> Context {
        Context {
            system_prompt: Some("system".to_string()),
            messages: vec![
                cell_ai_core::Message::User(UserMessage {
                    content: UserContent::Blocks(vec![
                        UserContentBlock::Text {
                            text: "hello".to_string(),
                            text_signature: None,
                        },
                        UserContentBlock::Image {
                            data: "abc".to_string(),
                            mime_type: "image/png".to_string(),
                        },
                    ]),
                    timestamp: 1,
                }),
                cell_ai_core::Message::Assistant(AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall {
                        id: "call-1|fc_call-1".to_string(),
                        name: "read".to_string(),
                        arguments: json!({"path":"README.md"}),
                        thought_signature: None,
                    }],
                    api: ApiId::new("openai-responses"),
                    provider: ProviderId::new("openai"),
                    model: "model-1".to_string(),
                    usage: Usage {
                        input: 0,
                        output: 0,
                        cache_read: 0,
                        cache_write: 0,
                        total_tokens: 0,
                        cost: UsageCost {
                            input: "0".to_string(),
                            output: "0".to_string(),
                            cache_read: "0".to_string(),
                            cache_write: "0".to_string(),
                            total: "0".to_string(),
                        },
                    },
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 2,
                }),
            ],
            tools: Some(vec![ToolDefinition {
                name: "read".to_string(),
                description: "Read files".to_string(),
                parameters: json!({"type":"object"}),
            }]),
        }
    }

    #[test]
    fn builds_openai_responses_request_shape() {
        let request = build_openai_responses_request(
            &model("openai-responses", "openai", "https://api.openai.com/v1"),
            &context(),
            &StreamOptions {
                max_tokens: Some(42),
                reasoning: Some(cell_ai_core::AiThinkingLevel::High),
                ..StreamOptions::default()
            },
        );

        assert_eq!(request["model"], json!("model-1"));
        assert_eq!(request["max_output_tokens"], json!(42));
        assert_eq!(request["input"][0]["role"], json!("developer"));
        assert_eq!(request["tools"][0]["name"], json!("read"));
        assert_eq!(request["reasoning"]["effort"], json!("high"));
    }

    #[test]
    fn builds_codex_request_with_instruction_separation() {
        let request = build_openai_codex_request(
            &model(
                "openai-codex-responses",
                "openai-codex",
                "https://chatgpt.com/backend-api",
            ),
            &context(),
            &StreamOptions {
                session_id: Some("session-1".to_string()),
                ..StreamOptions::default()
            },
        );

        assert_eq!(request["instructions"], json!("system"));
        assert_eq!(request["prompt_cache_key"], json!("session-1"));
        assert_eq!(request["tool_choice"], json!("auto"));
        assert_eq!(request["parallel_tool_calls"], json!(true));
    }

    #[test]
    fn builds_chat_completions_request_shape() {
        let request = build_chat_completions_request(
            &model(
                "openai-completions",
                "openrouter",
                "https://openrouter.ai/api/v1",
            ),
            &context(),
            &StreamOptions::default(),
        );

        assert_eq!(request["messages"][0]["role"], json!("system"));
        assert_eq!(request["messages"][1]["role"], json!("user"));
        assert_eq!(request["tools"][0]["function"]["name"], json!("read"));
    }

    #[test]
    fn maps_openai_responses_stream_events() {
        let model = model("openai-responses", "openai", "https://api.openai.com/v1");
        let mut output = initial_assistant_message(&model);
        let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
        drop(stream);
        let mut state = ResponsesStreamState::default();

        process_openai_responses_event(
            &json!({"type":"response.output_item.added","item":{"type":"message","id":"msg_1"}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("add message");
        process_openai_responses_event(
            &json!({"type":"response.output_text.delta","delta":"hello"}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("text delta");
        process_openai_responses_event(
            &json!({"type":"response.output_item.done","item":{"type":"message","id":"msg_1"}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("message done");
        process_openai_responses_event(
            &json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":4,"input_tokens_details":{"cached_tokens":2}}}}),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("completed");

        match &output.content[0] {
            AssistantContentBlock::Text {
                text,
                text_signature,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(text_signature.as_deref(), Some("msg_1"));
            }
            other => panic!("unexpected block: {other:?}"),
        }
        assert_eq!(output.stop_reason, StopReason::Stop);
        assert_eq!(output.usage.input, 8);
        assert_eq!(output.usage.cache_read, 2);
    }

    #[test]
    fn maps_chat_completions_tool_calls() {
        let model = model(
            "openai-completions",
            "openrouter",
            "https://openrouter.ai/api/v1",
        );
        let mut output = initial_assistant_message(&model);
        let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
        drop(stream);
        let mut state = ChatCompletionsState::default();

        process_chat_completions_event(
            &json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": { "name": "read", "arguments": "{\"path\":\"" }
                        }]
                    }
                }]
            }),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("first delta");
        process_chat_completions_event(
            &json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "README.md\"}" }
                        }]
                    }
                }]
            }),
            &mut output,
            &mut state,
            &mut sender,
            &model,
        )
        .expect("second delta");
        super::finalize_chat_completions_blocks(&mut output, &mut state, &mut sender)
            .expect("finalize");

        match &output.content[0] {
            AssistantContentBlock::ToolCall {
                name, arguments, ..
            } => {
                assert_eq!(name, "read");
                assert_eq!(arguments["path"], json!("README.md"));
            }
            other => panic!("unexpected block: {other:?}"),
        }
        assert_eq!(output.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn resolves_endpoint_urls_like_typescript() {
        assert_eq!(
            resolve_responses_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            resolve_chat_completions_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            resolve_codex_url("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn normalizes_codex_done_events() {
        let event = normalize_codex_event(json!({
            "type": "response.done",
            "response": { "status": "completed" }
        }));
        assert_eq!(event["type"], json!("response.completed"));
    }

    #[test]
    fn extracts_codex_account_id_from_jwt_claim() {
        let payload = URL_SAFE_NO_PAD.encode(
            json!({ "https://api.openai.com/auth": { "account_id": "acct_123" } })
                .to_string()
                .as_bytes(),
        );
        let token = format!("header.{payload}.signature");
        assert_eq!(
            extract_chatgpt_account_id(&token).as_deref(),
            Some("acct_123")
        );
    }
}
