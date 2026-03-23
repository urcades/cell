use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use pi_rust_ai_core::{AssistantMessage, Model, StopReason, Usage, UsageCost};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response};
use serde_json::Value;

pub(crate) fn initial_assistant_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
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
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: current_timestamp_ms(),
    }
}

pub(crate) fn update_usage(
    model: &Model,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> Usage {
    let input_cost = (input as f64 / 1_000_000.0) * model.cost.input;
    let output_cost = (output as f64 / 1_000_000.0) * model.cost.output;
    let cache_read_cost = (cache_read as f64 / 1_000_000.0) * model.cost.cache_read;
    let cache_write_cost = (cache_write as f64 / 1_000_000.0) * model.cost.cache_write;
    let total_cost = input_cost + output_cost + cache_read_cost + cache_write_cost;

    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost {
            input: format_cost(input_cost),
            output: format_cost(output_cost),
            cache_read: format_cost(cache_read_cost),
            cache_write: format_cost(cache_write_cost),
            total: format_cost(total_cost),
        },
    }
}

#[cfg(test)]
use std::collections::HashMap;

#[cfg(test)]
pub(crate) fn merge_header_maps(header_sets: &[Option<&HashMap<String, String>>]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for header_set in header_sets {
        let Some(header_set) = header_set else {
            continue;
        };
        for (key, value) in *header_set {
            insert_header(&mut headers, key, value);
        }
    }
    headers
}

pub(crate) fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
        return;
    };
    let Ok(value) = HeaderValue::from_str(value) else {
        return;
    };
    headers.insert(name, value);
}

pub(crate) async fn post_json(
    client: &Client,
    url: &str,
    headers: HeaderMap,
    body: &Value,
) -> Result<Response, String> {
    let response = client
        .post(url)
        .headers(headers)
        .json(body)
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let error_text = response
        .text()
        .await
        .unwrap_or_else(|_| String::new())
        .trim()
        .to_string();
    if error_text.is_empty() {
        Err(format!("request failed with status {status}"))
    } else {
        Err(format!("request failed with status {status}: {error_text}"))
    }
}

pub(crate) async fn consume_json_sse<F>(response: Response, mut on_event: F) -> Result<(), String>
where
    F: FnMut(Value) -> Result<(), String>,
{
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        if buffer.contains("\r\n") {
            buffer = buffer.replace("\r\n", "\n");
        }

        while let Some(separator) = buffer.find("\n\n") {
            let event = buffer[..separator].to_string();
            buffer = buffer[separator + 2..].to_string();

            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");

            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let parsed: Value = serde_json::from_str(&data).map_err(|error| {
                format!("failed to parse SSE event JSON: {error}. event={data}")
            })?;
            on_event(parsed)?;
        }
    }

    Ok(())
}

fn current_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn format_cost(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }

    let mut rendered = format!("{value:.12}");
    while rendered.contains('.') && rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use reqwest::header::HeaderMap;

    use super::{insert_header, merge_header_maps};

    #[test]
    fn merges_headers_with_later_values_winning() {
        let first = HashMap::from([
            ("accept".to_string(), "application/json".to_string()),
            ("x-test".to_string(), "first".to_string()),
        ]);
        let second = HashMap::from([("x-test".to_string(), "second".to_string())]);
        let headers = merge_header_maps(&[Some(&first), Some(&second)]);

        assert_eq!(
            headers.get("accept").and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers.get("x-test").and_then(|value| value.to_str().ok()),
            Some("second")
        );
    }

    #[test]
    fn drops_invalid_headers() {
        let mut headers = HeaderMap::new();
        insert_header(&mut headers, "not valid", "value");
        assert!(headers.is_empty());
    }
}
