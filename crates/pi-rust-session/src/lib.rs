mod manager;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

pub use manager::{
    RuntimeSessionContext, SessionBranchSummaryEntry, SessionCompactionEntry, SessionCustomEntry,
    SessionCustomMessageEntry, SessionEntry, SessionInfoEntry, SessionLabelEntry, SessionManager,
    SessionManagerError, SessionMessageEntry, SessionModelChangeEntry,
    SessionThinkingLevelChangeEntry,
};

pub const CURRENT_SESSION_VERSION: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryBase {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTreeNode {
    pub entry: Value,
    pub children: Vec<SessionTreeNode>,
    pub label: Option<String>,
}

#[derive(Debug, Error)]
pub enum SessionSchemaError {
    #[error("session entry is not an object")]
    InvalidObject,
}

pub fn parse_session_entries(content: &str) -> Vec<Value> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(trimmed).ok()
        })
        .collect()
}

pub fn migrate_session_entries(entries: &mut [Value]) {
    let version = detect_version(entries);
    if version < 2 {
        migrate_v1_to_v2(entries);
    }
    if version < 3 {
        migrate_v2_to_v3(entries);
    }
}

pub fn parse_header(value: &Value) -> Result<Option<SessionHeader>, SessionSchemaError> {
    let Some(object) = value.as_object() else {
        return Err(SessionSchemaError::InvalidObject);
    };
    if object.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    Ok(serde_json::from_value::<SessionHeader>(value.clone()).ok())
}

pub fn parse_entry_base(value: &Value) -> Result<Option<SessionEntryBase>, SessionSchemaError> {
    let Some(object) = value.as_object() else {
        return Err(SessionSchemaError::InvalidObject);
    };
    if object.get("type").and_then(Value::as_str) == Some("session") {
        return Ok(None);
    }
    Ok(serde_json::from_value::<SessionEntryBase>(value.clone()).ok())
}

pub fn build_session_tree(entries: &[Value]) -> Vec<SessionTreeNode> {
    let mut labels_by_target = HashMap::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) == Some("label") {
            if let Some(target_id) = entry.get("targetId").and_then(Value::as_str) {
                match entry.get("label").and_then(Value::as_str) {
                    Some(label) => {
                        labels_by_target.insert(target_id.to_string(), label.to_string());
                    }
                    None => {
                        labels_by_target.remove(target_id);
                    }
                }
            }
        }
    }

    #[derive(Clone)]
    struct NodeRecord {
        entry: Value,
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        label: Option<String>,
    }

    let nodes = entries
        .iter()
        .filter_map(|entry| {
            let base = parse_entry_base(entry).ok().flatten()?;
            Some(NodeRecord {
                entry: entry.clone(),
                id: base.id.clone(),
                parent_id: base.parent_id.clone(),
                timestamp: base.timestamp,
                label: labels_by_target.get(&base.id).cloned(),
            })
        })
        .collect::<Vec<_>>();

    let mut index_by_id = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        index_by_id.insert(node.id.clone(), index);
    }

    let mut child_indices: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();

    for (index, node) in nodes.iter().enumerate() {
        match node.parent_id.as_deref() {
            None => roots.push(index),
            Some(parent_id) if parent_id == node.id => roots.push(index),
            Some(parent_id) => {
                if let Some(parent_index) = index_by_id.get(parent_id) {
                    child_indices.entry(*parent_index).or_default().push(index);
                } else {
                    roots.push(index);
                }
            }
        }
    }

    for children in child_indices.values_mut() {
        children.sort_by(|left, right| nodes[*left].timestamp.cmp(&nodes[*right].timestamp));
    }
    roots.sort_by(|left, right| nodes[*left].timestamp.cmp(&nodes[*right].timestamp));

    fn build_node(
        index: usize,
        nodes: &[NodeRecord],
        child_indices: &HashMap<usize, Vec<usize>>,
    ) -> SessionTreeNode {
        let record = &nodes[index];
        let children = child_indices
            .get(&index)
            .into_iter()
            .flat_map(|items| items.iter().copied())
            .map(|child_index| build_node(child_index, nodes, child_indices))
            .collect();

        SessionTreeNode {
            entry: record.entry.clone(),
            children,
            label: record.label.clone(),
        }
    }

    roots
        .into_iter()
        .map(|root_index| build_node(root_index, &nodes, &child_indices))
        .collect()
}

pub fn encode_session_dir_name(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let safe_path = trimmed
        .chars()
        .map(|char| match char {
            '/' | '\\' | ':' => '-',
            value => value,
        })
        .collect::<String>();
    format!("--{safe_path}--")
}

fn detect_version(entries: &[Value]) -> u32 {
    entries
        .iter()
        .find_map(|entry| parse_header(entry).ok().flatten())
        .and_then(|header| header.version)
        .unwrap_or(1)
}

fn migrate_v1_to_v2(entries: &mut [Value]) {
    let mut previous_id: Option<String> = None;

    for index in 0..entries.len() {
        let entry_type = entries[index]
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let first_kept_entry_id = if entry_type.as_deref() == Some("compaction") {
            entries[index]
                .get("firstKeptEntryIndex")
                .and_then(Value::as_u64)
                .and_then(|first_kept_index| {
                    entries
                        .get(first_kept_index as usize)
                        .and_then(|entry| entry.get("id").and_then(Value::as_str))
                        .map(str::to_owned)
                })
        } else {
            None
        };

        let Some(object) = entries[index].as_object_mut() else {
            continue;
        };
        if entry_type.as_deref() == Some("session") {
            object.insert("version".to_string(), Value::from(2));
            continue;
        }

        let generated_id = short_uuid();
        object.insert("id".to_string(), Value::from(generated_id.clone()));
        object.insert(
            "parentId".to_string(),
            previous_id.clone().map(Value::from).unwrap_or(Value::Null),
        );
        previous_id = Some(generated_id.clone());

        if entry_type.as_deref() == Some("compaction") {
            if object
                .get("firstKeptEntryIndex")
                .and_then(Value::as_u64)
                .is_some()
            {
                if let Some(target_id) = first_kept_entry_id {
                    object.insert("firstKeptEntryId".to_string(), Value::from(target_id));
                }
                object.remove("firstKeptEntryIndex");
            }
        }
    }
}

fn migrate_v2_to_v3(entries: &mut [Value]) {
    for entry in entries.iter_mut() {
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) == Some("session") {
            object.insert("version".to_string(), Value::from(3));
            continue;
        }

        if object.get("type").and_then(Value::as_str) == Some("message") {
            if let Some(role) = object
                .get_mut("message")
                .and_then(Value::as_object_mut)
                .and_then(|message| message.get_mut("role"))
            {
                if role.as_str() == Some("hookMessage") {
                    *role = Value::from("custom");
                }
            }
        }
    }
}

fn short_uuid() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CURRENT_SESSION_VERSION, build_session_tree, encode_session_dir_name,
        migrate_session_entries, parse_entry_base, parse_header, parse_session_entries,
    };

    #[test]
    fn skips_malformed_jsonl_lines() {
        let entries =
            parse_session_entries("{\"type\":\"session\"}\nnot-json\n{\"type\":\"message\"}");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn migrates_legacy_entries_to_current_version() {
        let mut entries = vec![
            json!({"type": "session", "id": "sess", "timestamp": "t", "cwd": "/tmp"}),
            json!({"type": "message", "timestamp": "t", "message": {"role": "hookMessage"}}),
            json!({"type": "compaction", "timestamp": "t", "firstKeptEntryIndex": 1}),
        ];

        migrate_session_entries(&mut entries);

        let header = parse_header(&entries[0])
            .expect("header parse")
            .expect("header exists");
        assert_eq!(header.version, Some(CURRENT_SESSION_VERSION));
        assert!(entries[1].get("id").is_some());
        assert_eq!(entries[1]["message"]["role"], json!("custom"));
        assert!(entries[2].get("firstKeptEntryId").is_some());
        assert!(entries[2].get("firstKeptEntryIndex").is_none());
    }

    #[test]
    fn parses_entry_base_for_non_header_entries() {
        let entry = json!({
            "type": "message",
            "id": "abc12345",
            "parentId": null,
            "timestamp": "2025-01-01T00:00:00.000Z"
        });

        let parsed = parse_entry_base(&entry)
            .expect("entry parse")
            .expect("entry exists");
        assert_eq!(parsed.entry_type, "message");
        assert_eq!(parsed.id, "abc12345");
        assert_eq!(parsed.parent_id, None);
    }

    #[test]
    fn builds_tree_with_labels_orphans_and_timestamp_sorting() {
        let entries = vec![
            json!({"type": "session", "id": "sess", "timestamp": "2025-01-01T00:00:00.000Z", "cwd": "/tmp"}),
            json!({"type": "message", "id": "root", "parentId": null, "timestamp": "2025-01-01T00:00:01.000Z"}),
            json!({"type": "message", "id": "child-b", "parentId": "root", "timestamp": "2025-01-01T00:00:03.000Z"}),
            json!({"type": "message", "id": "child-a", "parentId": "root", "timestamp": "2025-01-01T00:00:02.000Z"}),
            json!({"type": "label", "id": "label-1", "parentId": "child-b", "timestamp": "2025-01-01T00:00:04.000Z", "targetId": "child-a", "label": "bookmark"}),
            json!({"type": "message", "id": "orphan", "parentId": "missing", "timestamp": "2025-01-01T00:00:05.000Z"}),
            json!({"type": "message", "id": "self-root", "parentId": "self-root", "timestamp": "2025-01-01T00:00:06.000Z"}),
        ];

        let tree = build_session_tree(&entries);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0].entry["id"], json!("root"));
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[0].children[0].entry["id"], json!("child-a"));
        assert_eq!(tree[0].children[0].label.as_deref(), Some("bookmark"));
        assert_eq!(tree[0].children[1].entry["id"], json!("child-b"));
        assert_eq!(tree[1].entry["id"], json!("orphan"));
        assert_eq!(tree[2].entry["id"], json!("self-root"));
    }

    #[test]
    fn encodes_session_directory_names_like_typescript() {
        assert_eq!(
            encode_session_dir_name("/Users/edouard/Developer/pi"),
            "--Users-edouard-Developer-pi--"
        );
        assert_eq!(
            encode_session_dir_name("C:\\Users\\edouard\\Developer\\pi"),
            "--C--Users-edouard-Developer-pi--"
        );
    }
}
