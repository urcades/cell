use std::fs;
use std::path::{Path, PathBuf};

use cell_ai_core::{AssistantContentBlock, Message, UserContent, UserContentBlock};
use cell_session::SessionManager;
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("failed to write export: {0}")]
    Io(#[from] std::io::Error),
}

pub fn export_session_to_html(
    session: &SessionManager,
    output_path: Option<&Path>,
    system_prompt: Option<&str>,
) -> Result<PathBuf, ExportError> {
    let session_file = session.get_session_file().map(Path::to_path_buf);
    let output_path = match output_path {
        Some(path) => path.to_path_buf(),
        None => default_export_path(session_file.as_deref()),
    };

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let header = session.get_header();
    let session_name = session
        .get_session_name()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Unnamed session".to_string());
    let context = session.build_session_context();
    let stats = calculate_stats(&context.messages);
    let messages = context
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| build_rendered_message(index + 1, message))
        .collect::<Vec<_>>();
    let jsonl_lines = build_jsonl_lines(header, &context.messages);
    let jsonl_file_name = export_file_name(&session_name, &header.id);

    let html = render_page(RenderPageInput {
        header,
        session_name: &session_name,
        session_file: session_file.as_deref(),
        thinking_level: context.thinking_level.as_deref(),
        model: context.model.as_ref(),
        system_prompt,
        stats: &stats,
        messages: &messages,
        jsonl_lines: &jsonl_lines,
        jsonl_file_name: &jsonl_file_name,
    });

    fs::write(&output_path, html)?;
    Ok(output_path)
}

fn default_export_path(session_file: Option<&Path>) -> PathBuf {
    match session_file {
        Some(path) => path.with_extension("html"),
        None => PathBuf::from("session.html"),
    }
}

struct SessionStats {
    total_messages: usize,
    user_messages: usize,
    assistant_messages: usize,
    tool_results: usize,
    text_blocks: usize,
    thinking_blocks: usize,
    tool_calls: usize,
    total_tokens: u64,
}

struct RenderedMessage {
    index: usize,
    role: &'static str,
    title: String,
    subtitle: String,
    preview: String,
    html: String,
}

struct RenderPageInput<'a> {
    header: &'a cell_session::SessionHeader,
    session_name: &'a str,
    session_file: Option<&'a Path>,
    thinking_level: Option<&'a str>,
    model: Option<&'a (String, String)>,
    system_prompt: Option<&'a str>,
    stats: &'a SessionStats,
    messages: &'a [RenderedMessage],
    jsonl_lines: &'a [String],
    jsonl_file_name: &'a str,
}

fn calculate_stats(messages: &[Message]) -> SessionStats {
    let mut stats = SessionStats {
        total_messages: messages.len(),
        user_messages: 0,
        assistant_messages: 0,
        tool_results: 0,
        text_blocks: 0,
        thinking_blocks: 0,
        tool_calls: 0,
        total_tokens: 0,
    };

    for message in messages {
        match message {
            Message::User(user) => {
                stats.user_messages += 1;
                stats.text_blocks += count_user_content_blocks(&user.content);
            }
            Message::Assistant(assistant) => {
                stats.assistant_messages += 1;
                stats.total_tokens += assistant.usage.total_tokens;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text { .. } => {
                            stats.text_blocks += 1;
                        }
                        AssistantContentBlock::Thinking { .. } => {
                            stats.thinking_blocks += 1;
                        }
                        AssistantContentBlock::ToolCall { .. } => {
                            stats.tool_calls += 1;
                        }
                    }
                }
            }
            Message::ToolResult(tool_result) => {
                stats.tool_results += 1;
                stats.text_blocks += tool_result.content.len();
            }
        }
    }

    stats
}

fn count_user_content_blocks(content: &UserContent) -> usize {
    match content {
        UserContent::Text(_) => 1,
        UserContent::Blocks(blocks) => blocks.len(),
    }
}

fn build_rendered_message(index: usize, message: &Message) -> RenderedMessage {
    let raw_text = normalize_whitespace(&collect_message_parts(message).join(" "));
    let preview = truncate_preview(&raw_text, 160);
    let search_text = raw_text.to_lowercase();

    match message {
        Message::User(user) => {
            let subtitle = match &user.content {
                UserContent::Text(_) => "Plain text".to_string(),
                UserContent::Blocks(blocks) if blocks.len() == 1 => "1 block".to_string(),
                UserContent::Blocks(blocks) => format!("{} blocks", blocks.len()),
            };
            let html = render_user_message(index, user, &preview, &search_text);

            RenderedMessage {
                index,
                role: "user",
                title: "User".to_string(),
                subtitle,
                preview,
                html,
            }
        }
        Message::Assistant(assistant) => {
            let html = render_assistant_message(index, assistant, &preview, &search_text);
            RenderedMessage {
                index,
                role: "assistant",
                title: "Assistant".to_string(),
                subtitle: format!(
                    "{} blocks · {} tokens",
                    assistant.content.len(),
                    assistant.usage.total_tokens
                ),
                preview,
                html,
            }
        }
        Message::ToolResult(tool_result) => {
            let html = render_tool_result_message(index, tool_result, &preview, &search_text);
            RenderedMessage {
                index,
                role: "tool",
                title: "Tool Result".to_string(),
                subtitle: format!(
                    "{}{}",
                    tool_result.tool_name,
                    if tool_result.is_error {
                        " · error"
                    } else {
                        ""
                    }
                ),
                preview,
                html,
            }
        }
    }
}

fn collect_message_parts(message: &Message) -> Vec<String> {
    let mut parts = Vec::new();
    match message {
        Message::User(user) => collect_user_content_parts(&user.content, &mut parts),
        Message::Assistant(assistant) => {
            for block in &assistant.content {
                collect_assistant_block_parts(block, &mut parts);
            }
        }
        Message::ToolResult(tool_result) => {
            parts.push(tool_result.tool_call_id.clone());
            parts.push(tool_result.tool_name.clone());
            parts.push(if tool_result.is_error {
                "error".to_string()
            } else {
                "success".to_string()
            });
            if let Some(details) = &tool_result.details {
                parts.push(pretty_json(details));
            }
            collect_user_content_parts(
                &UserContent::Blocks(tool_result.content.clone()),
                &mut parts,
            );
        }
    }
    parts
}

fn collect_user_content_parts(content: &UserContent, parts: &mut Vec<String>) {
    match content {
        UserContent::Text(text) => parts.push(text.clone()),
        UserContent::Blocks(blocks) => {
            for block in blocks {
                collect_user_block_parts(block, parts);
            }
        }
    }
}

fn collect_user_block_parts(block: &UserContentBlock, parts: &mut Vec<String>) {
    match block {
        UserContentBlock::Text { text, .. } => parts.push(text.clone()),
        UserContentBlock::Image { mime_type, .. } => parts.push(mime_type.clone()),
    }
}

fn collect_assistant_block_parts(block: &AssistantContentBlock, parts: &mut Vec<String>) {
    match block {
        AssistantContentBlock::Text { text, .. } => parts.push(text.clone()),
        AssistantContentBlock::Thinking { thinking, .. } => parts.push(thinking.clone()),
        AssistantContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            parts.push(id.clone());
            parts.push(name.clone());
            parts.push(pretty_json(arguments));
        }
    }
}

fn render_user_message(
    index: usize,
    user: &cell_ai_core::UserMessage,
    preview: &str,
    search_text: &str,
) -> String {
    let body = render_user_content(&user.content);
    let subtitle = match &user.content {
        UserContent::Text(_) => "Plain text",
        UserContent::Blocks(blocks) if blocks.len() == 1 => "1 block",
        UserContent::Blocks(blocks) => {
            return render_message_card(
                index,
                "user",
                "User",
                &format!("{} blocks", blocks.len()),
                preview,
                search_text,
                &body,
                None,
                None,
            );
        }
    };

    render_message_card(
        index,
        "user",
        "User",
        subtitle,
        preview,
        search_text,
        &body,
        None,
        None,
    )
}

fn render_assistant_message(
    index: usize,
    assistant: &cell_ai_core::AssistantMessage,
    preview: &str,
    search_text: &str,
) -> String {
    let body = assistant
        .content
        .iter()
        .map(render_assistant_block)
        .collect::<Vec<_>>()
        .join("");
    let model = format!("{}/{}", assistant.provider.0, assistant.model);
    let metadata = format!(
        "<dl class=\"message-fields\"><div><dt>Model</dt><dd>{}</dd></div><div><dt>API</dt><dd>{}</dd></div><div><dt>Stop reason</dt><dd>{}</dd></div><div><dt>Tokens</dt><dd>{}</dd></div></dl>",
        escape_html(&model),
        escape_html(&assistant.api.0),
        escape_html(&format!("{:?}", assistant.stop_reason)),
        assistant.usage.total_tokens
    );

    render_message_card(
        index,
        "assistant",
        "Assistant",
        &format!("{} blocks", assistant.content.len()),
        preview,
        search_text,
        &format!("{metadata}{body}"),
        Some(&format!("{} tokens", assistant.usage.total_tokens)),
        None,
    )
}

fn render_tool_result_message(
    index: usize,
    tool_result: &cell_ai_core::ToolResultMessage,
    preview: &str,
    search_text: &str,
) -> String {
    let body = render_tool_result_content(tool_result);
    let status = if tool_result.is_error {
        "Error"
    } else {
        "Success"
    };
    let metadata = format!(
        "<dl class=\"message-fields\"><div><dt>Tool</dt><dd>{}</dd></div><div><dt>Call ID</dt><dd>{}</dd></div><div><dt>Status</dt><dd>{}</dd></div></dl>",
        escape_html(&tool_result.tool_name),
        escape_html(&tool_result.tool_call_id),
        status
    );

    render_message_card(
        index,
        "tool",
        "Tool Result",
        &tool_result.tool_name,
        preview,
        search_text,
        &format!("{metadata}{body}"),
        Some(status),
        Some(tool_result.is_error),
    )
}

fn render_message_card(
    index: usize,
    role: &str,
    title: &str,
    subtitle: &str,
    preview: &str,
    search_text: &str,
    body: &str,
    trailing_badge: Option<&str>,
    is_error: Option<bool>,
) -> String {
    let badge = trailing_badge
        .map(|value| format!("<span class=\"badge\">{}</span>", escape_html(value)))
        .unwrap_or_default();
    let error_class = is_error
        .map(|value| if value { " error" } else { "" })
        .unwrap_or("");

    format!(
        "<article class=\"message message-{role}{error_class}\" id=\"message-{index}\" data-message data-role=\"{role}\" data-search=\"{}\" tabindex=\"-1\"><header class=\"message-header\"><div><p class=\"eyebrow\">#{index:02}</p><h2>{}</h2><p class=\"message-subtitle\">{}</p></div><div class=\"message-badges\">{badge}</div></header><p class=\"message-preview\">{}</p><div class=\"message-body\">{}</div></article>",
        escape_html(search_text),
        escape_html(title),
        escape_html(subtitle),
        escape_html(preview),
        body
    )
}

fn render_user_content(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => format!(
            "<div class=\"content-stack\"><pre class=\"content-block text\">{}</pre></div>",
            escape_html(text)
        ),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(render_user_block)
            .collect::<Vec<_>>()
            .join(""),
    }
}

fn render_user_block(block: &UserContentBlock) -> String {
    match block {
        UserContentBlock::Text { text, .. } => format!(
            "<div class=\"content-stack\"><pre class=\"content-block text\">{}</pre></div>",
            escape_html(text)
        ),
        UserContentBlock::Image { data, mime_type } => format!(
            "<figure class=\"content-block image\"><img src=\"data:{};base64,{}\" alt=\"Embedded image\" loading=\"lazy\" /><figcaption>{}</figcaption></figure>",
            escape_html(mime_type),
            escape_html(data),
            escape_html(mime_type)
        ),
    }
}

fn render_assistant_block(block: &AssistantContentBlock) -> String {
    match block {
        AssistantContentBlock::Text { text, .. } => format!(
            "<div class=\"content-stack assistant-block text\"><pre class=\"content-block text\">{}</pre></div>",
            escape_html(text)
        ),
        AssistantContentBlock::Thinking { thinking, .. } => format!(
            "<details class=\"assistant-block thinking\" open><summary><span>Thinking</span><span class=\"badge\">{} chars</span></summary><pre class=\"content-block text\">{}</pre></details>",
            thinking.chars().count(),
            escape_html(thinking)
        ),
        AssistantContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            let arguments = pretty_json(arguments);
            format!(
                "<details class=\"assistant-block tool-call\"><summary><span>Tool Call</span><span class=\"badge\">{}</span><span class=\"badge\">{}</span></summary><dl class=\"message-fields\"><div><dt>ID</dt><dd>{}</dd></div><div><dt>Name</dt><dd>{}</dd></div></dl><pre class=\"content-block text\">{}</pre></details>",
                escape_html(name),
                escape_html(id),
                escape_html(id),
                escape_html(name),
                escape_html(&arguments)
            )
        }
    }
}

fn render_tool_result_content(tool_result: &cell_ai_core::ToolResultMessage) -> String {
    let mut html = String::new();
    html.push_str(&format!(
        "<dl class=\"message-fields\"><div><dt>Timestamp</dt><dd>{}</dd></div></dl>",
        tool_result.timestamp
    ));

    if let Some(details) = &tool_result.details {
        html.push_str(&format!(
            "<details class=\"assistant-block tool-result-details\"><summary><span>Details</span><span class=\"badge\">JSON</span></summary><pre class=\"content-block text\">{}</pre></details>",
            escape_html(&pretty_json(details))
        ));
    }

    html.push_str("<div class=\"content-stack tool-result-content\">");
    for block in &tool_result.content {
        html.push_str(&render_user_block(block));
    }
    html.push_str("</div>");
    html
}

fn build_jsonl_lines(header: &cell_session::SessionHeader, messages: &[Message]) -> Vec<String> {
    let mut lines = Vec::with_capacity(messages.len() + 1);
    lines.push(serde_json::to_string(header).expect("serialize session header"));
    for (index, message) in messages.iter().enumerate() {
        lines.push(
            serde_json::to_string(&message_to_jsonl_value(index + 1, message))
                .expect("serialize session message"),
        );
    }
    lines
}

fn message_to_jsonl_value(index: usize, message: &Message) -> Value {
    match message {
        Message::User(user) => json!({
            "index": index,
            "type": "message",
            "role": "user",
            "timestamp": user.timestamp,
            "content": user_content_to_value(&user.content),
        }),
        Message::Assistant(assistant) => json!({
            "index": index,
            "type": "message",
            "role": "assistant",
            "timestamp": assistant.timestamp,
            "api": assistant.api,
            "provider": assistant.provider,
            "model": assistant.model,
            "usage": assistant.usage,
            "stopReason": assistant.stop_reason,
            "errorMessage": assistant.error_message,
            "content": assistant
                .content
                .iter()
                .map(assistant_block_to_value)
                .collect::<Vec<_>>(),
        }),
        Message::ToolResult(tool_result) => json!({
            "index": index,
            "type": "tool_result",
            "timestamp": tool_result.timestamp,
            "toolCallId": tool_result.tool_call_id,
            "toolName": tool_result.tool_name,
            "isError": tool_result.is_error,
            "details": tool_result.details,
            "content": tool_result
                .content
                .iter()
                .map(user_block_to_value)
                .collect::<Vec<_>>(),
        }),
    }
}

fn user_content_to_value(content: &UserContent) -> Value {
    match content {
        UserContent::Text(text) => json!({ "type": "text", "text": text }),
        UserContent::Blocks(blocks) => {
            Value::Array(blocks.iter().map(user_block_to_value).collect::<Vec<_>>())
        }
    }
}

fn user_block_to_value(block: &UserContentBlock) -> Value {
    match block {
        UserContentBlock::Text { text, .. } => json!({
            "type": "text",
            "text": text,
        }),
        UserContentBlock::Image { data, mime_type } => json!({
            "type": "image",
            "data": data,
            "mimeType": mime_type,
        }),
    }
}

fn assistant_block_to_value(block: &AssistantContentBlock) -> Value {
    match block {
        AssistantContentBlock::Text { text, .. } => json!({
            "type": "text",
            "text": text,
        }),
        AssistantContentBlock::Thinking { thinking, .. } => json!({
            "type": "thinking",
            "thinking": thinking,
        }),
        AssistantContentBlock::ToolCall {
            id,
            name,
            arguments,
            ..
        } => json!({
            "type": "toolCall",
            "id": id,
            "name": name,
            "arguments": arguments,
        }),
    }
}

fn render_page(input: RenderPageInput<'_>) -> String {
    let session_file = input
        .session_file
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Not set".to_string());
    let model = input
        .model
        .map(|(provider, model)| format!("{provider}/{model}"))
        .unwrap_or_else(|| "Not set".to_string());
    let thinking_level = input.thinking_level.unwrap_or("off");
    let session_header = format!(
        "<header class=\"hero panel\"><div><p class=\"eyebrow\">Session export</p><h1>{}</h1><p class=\"lede\">A searchable transcript viewer with navigation, filters, and a JSONL download.</p></div><div class=\"hero-actions\"><a class=\"button primary\" href=\"#transcript\">Jump to transcript</a><button class=\"button\" id=\"download-jsonl\" type=\"button\" data-download-file=\"{}\">Download JSONL</button></div></header>",
        escape_html(input.session_name),
        escape_html(input.jsonl_file_name)
    );

    let session_meta = format!(
        "<section class=\"panel session-meta\"><h2>Session</h2><dl class=\"meta-grid\"><div><dt>Session ID</dt><dd>{}</dd></div><div><dt>Created</dt><dd>{}</dd></div><div><dt>CWD</dt><dd>{}</dd></div><div><dt>Session file</dt><dd>{}</dd></div><div><dt>Model</dt><dd>{}</dd></div><div><dt>Thinking</dt><dd>{}</dd></div></dl></section>",
        escape_html(input.header.id.as_str()),
        escape_html(input.header.timestamp.as_str()),
        escape_html(input.header.cwd.as_str()),
        escape_html(&session_file),
        escape_html(&model),
        escape_html(thinking_level)
    );

    let sidebar = render_sidebar(
        input.stats,
        input.messages,
        &session_meta,
        input.jsonl_file_name,
    );
    let stats = render_stats_cards(input.stats);
    let prompt_section = input.system_prompt.filter(|value| !value.trim().is_empty()).map(|prompt| {
        format!(
            "<section class=\"panel system-prompt\"><details open><summary><span>System prompt</span><span class=\"badge\">{} chars</span></summary><pre class=\"content-block text\">{}</pre></details></section>",
            prompt.chars().count(),
            escape_html(prompt)
        )
    }).unwrap_or_default();
    let transcript = if input.messages.is_empty() {
        "<section class=\"panel transcript\" id=\"transcript\"><div class=\"empty-state\">This session has no transcript messages yet.</div></section>".to_string()
    } else {
        format!(
            "<section class=\"transcript\" id=\"transcript\">{}</section>",
            input
                .messages
                .iter()
                .map(|message| message.html.as_str())
                .collect::<Vec<_>>()
                .join("")
        )
    };
    let jsonl_payload = escape_json_script(
        &serde_json::to_string(input.jsonl_lines).expect("serialize jsonl payload"),
    );

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" /><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" /><title>cell session export</title><style>{}</style></head><body><div class=\"shell\"><aside class=\"sidebar panel\">{}</aside><main class=\"workspace\">{}{}{}<section class=\"panel transcript-shell\">{}</section></main></div><script id=\"session-jsonl\" type=\"application/json\">{}</script><script>{}</script></body></html>",
        DEFAULT_EXPORT_STYLES,
        sidebar,
        session_header,
        stats,
        prompt_section,
        transcript,
        jsonl_payload,
        INLINE_SCRIPT
    )
}

fn render_sidebar(
    stats: &SessionStats,
    messages: &[RenderedMessage],
    session_meta: &str,
    jsonl_file_name: &str,
) -> String {
    let visible_count = messages.len();
    let nav_count = messages.len();
    format!(
        "<div class=\"sidebar-stack\"><section class=\"panel sidebar-card\"><div class=\"panel-head\"><div><p class=\"eyebrow\">Session overview</p><h2>Transcript tools</h2></div><button class=\"button primary\" id=\"sidebar-download-jsonl\" type=\"button\" data-download-file=\"{}\">JSONL</button></div><p class=\"sidebar-summary\" id=\"visible-count\">Showing {} of {} messages</p></section>{}{}{} </div>",
        escape_html(jsonl_file_name),
        visible_count,
        nav_count,
        session_meta,
        render_sidebar_search(),
        render_filter_controls(stats, messages)
    )
}

fn render_sidebar_search() -> String {
    format!(
        "<section class=\"panel sidebar-card controls\"><label class=\"field-label\" for=\"message-search\">Search transcript</label><input id=\"message-search\" type=\"search\" placeholder=\"Search text, tool names, thinking, IDs\" autocomplete=\"off\" /><div class=\"filter-row\" role=\"group\" aria-label=\"Transcript filters\"><button class=\"filter-button active\" type=\"button\" data-filter=\"all\">All</button><button class=\"filter-button\" type=\"button\" data-filter=\"user\">User</button><button class=\"filter-button\" type=\"button\" data-filter=\"assistant\">Assistant</button><button class=\"filter-button\" type=\"button\" data-filter=\"tool\">Tool</button></div></section>"
    )
}

fn render_filter_controls(stats: &SessionStats, messages: &[RenderedMessage]) -> String {
    let counts = format!(
        "<section class=\"panel sidebar-card quick-stats\"><div class=\"panel-head\"><div><p class=\"eyebrow\">Matches</p><h2>Transcript filters</h2></div></div><dl class=\"meta-grid compact\"><div><dt>Total</dt><dd>{}</dd></div><div><dt>User</dt><dd>{}</dd></div><div><dt>Assistant</dt><dd>{}</dd></div><div><dt>Tool</dt><dd>{}</dd></div></dl></section>",
        stats.total_messages, stats.user_messages, stats.assistant_messages, stats.tool_results
    );

    let mut nav = String::new();
    nav.push_str("<section class=\"panel sidebar-card transcript-nav\"><div class=\"panel-head\"><div><p class=\"eyebrow\">Navigation</p><h2>Transcript</h2></div></div><ol id=\"transcript-nav\" class=\"nav-list\">");
    if messages.is_empty() {
        nav.push_str("<li class=\"nav-empty\">No messages</li>");
    } else {
        for message in messages {
            nav.push_str(&format!(
                "<li><a class=\"nav-item\" href=\"#message-{}\" data-target=\"message-{}\" data-role=\"{}\"><span class=\"nav-index\">#{:02}</span><span class=\"nav-title\">{}</span><span class=\"nav-subtitle\">{}</span><span class=\"nav-preview\">{}</span></a></li>",
                message.index,
                message.index,
                message.role,
                message.index,
                escape_html(&message.title),
                escape_html(&message.subtitle),
                escape_html(&message.preview)
            ));
        }
    }
    nav.push_str("</ol></section>");
    format!("{counts}{nav}")
}

fn render_stats_cards(stats: &SessionStats) -> String {
    format!(
        "<section class=\"panel stats-panel\"><div class=\"panel-head\"><div><p class=\"eyebrow\">Session stats</p><h2>Counts</h2></div></div><div class=\"stats-grid\"><div class=\"stat-card\"><span class=\"stat-label\">Messages</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">User</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">Assistant</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">Tool results</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">Thinking blocks</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">Tool calls</span><strong>{}</strong></div><div class=\"stat-card\"><span class=\"stat-label\">Tokens</span><strong>{}</strong></div></div></section>",
        stats.total_messages,
        stats.user_messages,
        stats.assistant_messages,
        stats.tool_results,
        stats.thinking_blocks,
        stats.tool_calls,
        stats.total_tokens
    )
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_preview(value: &str, max_chars: usize) -> String {
    let normalized = normalize_whitespace(value);
    let mut chars = normalized.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_json_script(value: &str) -> String {
    value
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

fn export_file_name(session_name: &str, session_id: &str) -> String {
    let base = if session_name.trim().is_empty() {
        session_id.trim()
    } else {
        session_name.trim()
    };
    let mut slug = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("session-{session_id}.jsonl")
    } else {
        format!("{slug}.jsonl")
    }
}

const DEFAULT_EXPORT_STYLES: &str = r#"
:root {
  color-scheme: dark;
  --bg: #07111f;
  --bg-alt: #0b1424;
  --panel: rgba(12, 18, 32, 0.84);
  --panel-strong: #101a30;
  --panel-border: rgba(148, 163, 184, 0.22);
  --panel-border-strong: rgba(148, 163, 184, 0.35);
  --text: #e5eefb;
  --muted: #95a3b9;
  --accent: #8b5cf6;
  --accent-2: #38bdf8;
  --user: #60a5fa;
  --assistant: #34d399;
  --tool: #f59e0b;
  --danger: #f87171;
}
* { box-sizing: border-box; }
html { scroll-behavior: smooth; }
body {
  margin: 0;
  min-height: 100vh;
  color: var(--text);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  background:
    radial-gradient(circle at top left, rgba(59, 130, 246, 0.18), transparent 34%),
    radial-gradient(circle at top right, rgba(168, 85, 247, 0.16), transparent 28%),
    linear-gradient(180deg, var(--bg) 0%, var(--bg-alt) 100%);
}
a { color: inherit; text-decoration: none; }
button, input { font: inherit; }
button {
  cursor: pointer;
  border: 0;
}
.shell {
  display: grid;
  grid-template-columns: 352px minmax(0, 1fr);
  gap: 24px;
  padding: 24px;
}
.panel {
  background: var(--panel);
  border: 1px solid var(--panel-border);
  border-radius: 20px;
  box-shadow: 0 20px 60px rgba(2, 6, 23, 0.32);
  backdrop-filter: blur(18px);
}
.sidebar {
  position: sticky;
  top: 24px;
  height: calc(100vh - 48px);
  overflow: auto;
  padding: 18px;
}
.sidebar-stack {
  display: grid;
  gap: 16px;
}
.sidebar-card,
.session-meta,
.stats-panel,
.system-prompt,
.hero {
  padding: 18px;
}
.hero {
  display: flex;
  justify-content: space-between;
  gap: 20px;
  align-items: flex-start;
  margin-bottom: 16px;
}
.hero h1,
.panel h2 {
  margin: 0;
  font-size: 1.35rem;
}
.eyebrow {
  margin: 0 0 8px;
  text-transform: uppercase;
  letter-spacing: 0.18em;
  font-size: 0.72rem;
  color: var(--muted);
}
.lede,
.sidebar-summary,
.message-preview,
.message-subtitle {
  color: var(--muted);
}
.hero-actions {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
}
.button,
.filter-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  padding: 10px 14px;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.1);
  color: var(--text);
  border: 1px solid var(--panel-border-strong);
  transition: transform 120ms ease, background 120ms ease, border-color 120ms ease;
}
.button:hover,
.filter-button:hover,
.nav-item:hover {
  transform: translateY(-1px);
  border-color: rgba(255, 255, 255, 0.24);
}
.button.primary,
.filter-button.active {
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.82), rgba(56, 189, 248, 0.78));
  color: white;
}
.meta-grid,
.message-fields {
  display: grid;
  gap: 12px;
}
.meta-grid {
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
}
.meta-grid.compact {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}
.meta-grid dt,
.message-fields dt {
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.16em;
  color: var(--muted);
}
.meta-grid dd,
.message-fields dd {
  margin: 4px 0 0;
  word-break: break-word;
}
.stats-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(124px, 1fr));
  gap: 12px;
}
.stat-card {
  padding: 14px;
  border-radius: 16px;
  background: rgba(15, 23, 42, 0.72);
  border: 1px solid rgba(148, 163, 184, 0.16);
}
.stat-label {
  display: block;
  color: var(--muted);
  font-size: 0.75rem;
  margin-bottom: 10px;
}
.stat-card strong {
  font-size: 1.3rem;
}
.field-label {
  display: block;
  margin-bottom: 8px;
  color: var(--muted);
}
input[type="search"] {
  width: 100%;
  padding: 12px 14px;
  border-radius: 14px;
  border: 1px solid var(--panel-border-strong);
  background: rgba(15, 23, 42, 0.72);
  color: var(--text);
}
.filter-row {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 12px;
}
.filter-button {
  padding: 8px 12px;
  font-size: 0.92rem;
}
.nav-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: grid;
  gap: 8px;
}
.nav-item {
  display: grid;
  gap: 6px;
  padding: 12px 14px;
  border-radius: 16px;
  background: rgba(15, 23, 42, 0.72);
  border: 1px solid rgba(148, 163, 184, 0.16);
}
.nav-item.active {
  border-color: rgba(56, 189, 248, 0.6);
  background: rgba(30, 41, 59, 0.95);
}
.nav-index,
.nav-title,
.nav-subtitle,
.nav-preview {
  display: block;
}
.nav-index {
  color: var(--muted);
  font-size: 0.72rem;
}
.nav-title {
  font-weight: 650;
}
.nav-subtitle,
.nav-preview {
  color: var(--muted);
  font-size: 0.92rem;
}
.workspace {
  min-width: 0;
}
.transcript-shell {
  padding: 18px;
}
.transcript {
  display: grid;
  gap: 16px;
}
.message {
  border-radius: 20px;
  padding: 18px;
  background: rgba(15, 23, 42, 0.82);
  border: 1px solid rgba(148, 163, 184, 0.18);
  border-left-width: 5px;
  outline: none;
  scroll-margin-top: 18px;
}
.message[data-role="user"] {
  border-left-color: var(--user);
}
.message[data-role="assistant"] {
  border-left-color: var(--assistant);
}
.message[data-role="tool"] {
  border-left-color: var(--tool);
}
.message.error {
  border-left-color: var(--danger);
}
.message.active {
  border-color: rgba(56, 189, 248, 0.58);
  box-shadow: 0 0 0 1px rgba(56, 189, 248, 0.18), 0 14px 34px rgba(15, 23, 42, 0.38);
}
.message-header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
  margin-bottom: 12px;
}
.message-header h2 {
  margin: 0;
  font-size: 1.08rem;
}
.message-preview {
  margin: 0 0 14px;
}
.message-badges {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
}
.badge {
  display: inline-flex;
  align-items: center;
  padding: 5px 9px;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.12);
  color: var(--text);
  border: 1px solid rgba(148, 163, 184, 0.18);
  font-size: 0.78rem;
}
.content-stack {
  display: grid;
  gap: 12px;
}
.content-block,
.assistant-block pre,
.system-prompt pre {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  line-height: 1.6;
}
.content-block.text {
  padding: 14px;
  border-radius: 16px;
  background: rgba(2, 6, 23, 0.45);
  border: 1px solid rgba(148, 163, 184, 0.14);
}
.content-block.image {
  margin: 0;
  padding: 14px;
  border-radius: 16px;
  background: rgba(2, 6, 23, 0.45);
  border: 1px solid rgba(148, 163, 184, 0.14);
}
.content-block.image img {
  width: 100%;
  height: auto;
  border-radius: 12px;
  display: block;
}
.content-block.image figcaption {
  margin-top: 10px;
  color: var(--muted);
  font-size: 0.9rem;
}
details {
  border-radius: 16px;
  background: rgba(2, 6, 23, 0.45);
  border: 1px solid rgba(148, 163, 184, 0.14);
  padding: 12px 14px;
}
details summary {
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 8px;
  list-style: none;
}
details summary::-webkit-details-marker {
  display: none;
}
.assistant-block.tool-call summary,
.assistant-block.thinking summary,
.assistant-block.tool-result-details summary {
  font-weight: 650;
}
.assistant-block .message-fields,
.tool-result-content {
  margin-top: 12px;
}
.empty-state {
  padding: 28px;
  text-align: center;
  color: var(--muted);
}
.hidden {
  display: none !important;
}
.message-index {
  display: none;
}
.panel-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
  margin-bottom: 14px;
}
.compact .meta-grid {
  grid-template-columns: 1fr;
}
@media (max-width: 1100px) {
  .shell {
    grid-template-columns: 1fr;
  }
  .sidebar {
    position: static;
    height: auto;
  }
}
"#;

const INLINE_SCRIPT: &str = r#"
(() => {
  const cards = Array.from(document.querySelectorAll('[data-message]'));
  const navItems = Array.from(document.querySelectorAll('[data-target]'));
  const search = document.getElementById('message-search');
  const filterButtons = Array.from(document.querySelectorAll('[data-filter]'));
  const visibleCount = document.getElementById('visible-count');
  const transcript = document.getElementById('transcript');
  const jsonlNode = document.getElementById('session-jsonl');
  const downloadButtons = Array.from(document.querySelectorAll('[data-download-file]'));
  const jsonlLines = jsonlNode ? JSON.parse(jsonlNode.textContent || '[]') : [];
  let activeFilter = 'all';
  let activeQuery = '';

  const setActiveMessage = (id) => {
    navItems.forEach((item) => item.classList.toggle('active', item.dataset.target === id));
    cards.forEach((card) => card.classList.toggle('active', card.id === id));
  };

  const updateVisibleCount = () => {
    const visible = cards.filter((card) => !card.classList.contains('hidden')).length;
    if (visibleCount) {
      visibleCount.textContent = `Showing ${visible} of ${cards.length} messages`;
    }
  };

  const applyFilters = () => {
    const query = activeQuery.trim().toLowerCase();
    cards.forEach((card) => {
      const role = card.dataset.role || '';
      const searchText = card.dataset.search || '';
      const matchesRole = activeFilter === 'all' || role === activeFilter;
      const matchesQuery = query === '' || searchText.includes(query);
      const visible = matchesRole && matchesQuery;
      card.classList.toggle('hidden', !visible);
      const nav = navItems.find((item) => item.dataset.target === card.id);
      if (nav) {
        nav.classList.toggle('hidden', !visible);
      }
    });
    updateVisibleCount();
  };

  if (search) {
    search.addEventListener('input', (event) => {
      activeQuery = event.target.value || '';
      applyFilters();
    });
  }

  filterButtons.forEach((button) => {
    button.addEventListener('click', () => {
      activeFilter = button.dataset.filter || 'all';
      filterButtons.forEach((candidate) => {
        candidate.classList.toggle('active', candidate === button);
      });
      applyFilters();
    });
  });

  navItems.forEach((item) => {
    item.addEventListener('click', (event) => {
      const target = item.dataset.target;
      if (!target) {
        return;
      }
      event.preventDefault();
      const card = document.getElementById(target);
      if (card) {
        card.scrollIntoView({ behavior: 'smooth', block: 'start' });
        card.focus({ preventScroll: true });
        setActiveMessage(target);
      }
    });
  });

  cards.forEach((card) => {
    card.addEventListener('click', () => setActiveMessage(card.id));
  });

  if ('IntersectionObserver' in window && cards.length > 0) {
    const observer = new IntersectionObserver((entries) => {
      const visibleEntries = entries.filter((entry) => entry.isIntersecting);
      if (visibleEntries.length === 0) {
        return;
      }
      visibleEntries.sort((left, right) => right.intersectionRatio - left.intersectionRatio);
      setActiveMessage(visibleEntries[0].target.id);
    }, { rootMargin: '-20% 0px -58% 0px', threshold: [0.1, 0.25, 0.5] });
    cards.forEach((card) => observer.observe(card));
  }

  const triggerDownload = (filename) => {
    const blob = new Blob([jsonlLines.join('\n')], { type: 'application/x-ndjson;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  downloadButtons.forEach((button) => {
    button.addEventListener('click', () => {
      triggerDownload(button.dataset.downloadFile || 'session.jsonl');
    });
  });

  applyFilters();
  if (transcript && cards.length > 0) {
    setActiveMessage(cards[0].id);
  }
})();
"#;

#[cfg(test)]
mod tests {
    use std::fs;

    use cell_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, Message, ProviderId, StopReason, Usage,
        UsageCost, UserContent, UserMessage,
    };
    use cell_session::SessionManager;
    use serde_json::json;
    use tempfile::tempdir;

    use super::export_session_to_html;

    fn assistant_message() -> AssistantMessage {
        AssistantMessage {
            content: vec![
                AssistantContentBlock::Thinking {
                    thinking: "I should inspect the available files first.".to_string(),
                    thinking_signature: None,
                },
                AssistantContentBlock::ToolCall {
                    id: "tool-call-1".to_string(),
                    name: "list_files".to_string(),
                    arguments: json!({
                        "path": "/tmp/project",
                        "recursive": true,
                    }),
                    thought_signature: None,
                },
                AssistantContentBlock::Text {
                    text: "Done.".to_string(),
                    text_signature: None,
                },
            ],
            api: ApiId::new("openai-responses"),
            provider: ProviderId::new("openai"),
            model: "gpt-5.1-codex".to_string(),
            usage: Usage {
                input: 1,
                output: 3,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 4,
                cost: UsageCost {
                    input: "0".to_string(),
                    output: "0".to_string(),
                    cache_read: "0".to_string(),
                    cache_write: "0".to_string(),
                    total: "0".to_string(),
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    #[test]
    fn exports_session_branch_to_html() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::create(tempdir.path(), None).expect("session");
        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("hello".to_string()),
                timestamp: 0,
            }))
            .expect("user");
        session
            .append_message(Message::Assistant(assistant_message()))
            .expect("assistant");

        let output_path =
            export_session_to_html(&session, None, Some("system prompt")).expect("export");
        let html = fs::read_to_string(output_path).expect("html");
        assert!(html.contains("system prompt"));
        assert!(html.contains("hello"));
        assert!(html.contains("Done."));
        assert!(html.contains("Search transcript"));
        assert!(html.contains("Download JSONL"));
        assert!(html.contains("transcript-nav"));
        assert!(html.contains("session-jsonl"));
        assert!(html.contains("Thinking"));
        assert!(html.contains("Tool Call"));
    }

    #[test]
    fn renders_tool_results_and_inline_images() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::create(tempdir.path(), None).expect("session");
        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Blocks(vec![
                    cell_ai_core::UserContentBlock::Text {
                        text: "user text".to_string(),
                        text_signature: None,
                    },
                    cell_ai_core::UserContentBlock::Image {
                        data: "ZmFrZQ==".to_string(),
                        mime_type: "image/png".to_string(),
                    },
                ]),
                timestamp: 1,
            }))
            .expect("user");
        session
            .append_message(Message::ToolResult(cell_ai_core::ToolResultMessage {
                tool_call_id: "tool-call-1".to_string(),
                tool_name: "list_files".to_string(),
                content: vec![cell_ai_core::UserContentBlock::Text {
                    text: "file-a".to_string(),
                    text_signature: None,
                }],
                details: Some(json!({"count": 1})),
                is_error: false,
                timestamp: 2,
            }))
            .expect("tool result");

        let output_path = export_session_to_html(&session, None, None).expect("export");
        let html = fs::read_to_string(output_path).expect("html");
        assert!(html.contains("data:image/png;base64,ZmFrZQ=="));
        assert!(html.contains("Tool Result"));
        assert!(html.contains("tool-call-1"));
        assert!(html.contains("&quot;count&quot;: 1"));
        assert!(html.contains("application/x-ndjson"));
        assert!(html.contains("tool_result"));
    }
}
