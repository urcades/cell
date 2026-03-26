use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use cell_ai_core::{Context, Message, UserContent, UserContentBlock, UserMessage};
use cell_config::get_sessions_dir;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::{
    CURRENT_SESSION_VERSION, SessionHeader, build_session_tree, encode_session_dir_name,
    migrate_session_entries, parse_entry_base, parse_header, parse_session_entries,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(SessionMessageEntry),
    ThinkingLevelChange(SessionThinkingLevelChangeEntry),
    ModelChange(SessionModelChangeEntry),
    Compaction(SessionCompactionEntry),
    BranchSummary(SessionBranchSummaryEntry),
    Custom(SessionCustomEntry),
    CustomMessage(SessionCustomMessageEntry),
    Label(SessionLabelEntry),
    SessionInfo(SessionInfoEntry),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: Message,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionThinkingLevelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLabelEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub target_id: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBranchSummaryEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCustomEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCustomMessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSessionContext {
    pub messages: Vec<Message>,
    pub thinking_level: Option<String>,
    pub model: Option<(String, String)>,
}

const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

#[derive(Debug, Error)]
pub enum SessionManagerError {
    #[error("failed to read session: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse session entry: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid session file")]
    InvalidSessionFile,
    #[error("session entry not found: {0}")]
    MissingEntry(String),
}

#[derive(Clone, Debug)]
pub struct SessionManager {
    cwd: PathBuf,
    session_dir: PathBuf,
    session_file: Option<PathBuf>,
    header: SessionHeader,
    entries: Vec<Value>,
    leaf_id: Option<String>,
    persist: bool,
    flushed: bool,
}

impl SessionManager {
    pub fn create(
        cwd: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
    ) -> Result<Self, SessionManagerError> {
        let cwd = cwd.into();
        let session_dir = session_dir.unwrap_or_else(|| default_session_dir(&cwd));
        fs::create_dir_all(&session_dir)?;

        let timestamp = now_iso_timestamp();
        let session_id = Uuid::new_v4().to_string();
        let file_timestamp = timestamp.replace(':', "-").replace('.', "-");
        let session_file = session_dir.join(format!("{file_timestamp}_{session_id}.jsonl"));
        let header = SessionHeader {
            entry_type: "session".to_string(),
            version: Some(CURRENT_SESSION_VERSION),
            id: session_id,
            timestamp,
            cwd: cwd.to_string_lossy().to_string(),
            parent_session: None,
        };

        Ok(Self {
            cwd,
            session_dir,
            session_file: Some(session_file),
            header,
            entries: Vec::new(),
            leaf_id: None,
            persist: true,
            flushed: false,
        })
    }

    pub fn in_memory(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        Self {
            session_dir: default_session_dir(&cwd),
            header: SessionHeader {
                entry_type: "session".to_string(),
                version: Some(CURRENT_SESSION_VERSION),
                id: Uuid::new_v4().to_string(),
                timestamp: now_iso_timestamp(),
                cwd: cwd.to_string_lossy().to_string(),
                parent_session: None,
            },
            cwd,
            session_file: None,
            entries: Vec::new(),
            leaf_id: None,
            persist: false,
            flushed: false,
        }
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionManagerError> {
        let path = path.into();
        let content = fs::read_to_string(&path)?;
        if !content_has_valid_header(&content) {
            return Err(SessionManagerError::InvalidSessionFile);
        }

        let mut raw_entries = parse_session_entries(&content);
        let original_entries = raw_entries.clone();
        migrate_session_entries(&mut raw_entries);
        let header = raw_entries
            .iter()
            .find_map(|entry| parse_header(entry).ok().flatten())
            .ok_or(SessionManagerError::InvalidSessionFile)?;
        let entries = raw_entries
            .into_iter()
            .filter(|entry| parse_header(entry).ok().flatten().is_none())
            .collect::<Vec<_>>();
        let cwd = PathBuf::from(&header.cwd);
        let session_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_session_dir(&cwd));
        let leaf_id = last_entry_id(&entries);

        let manager = Self {
            cwd,
            session_dir,
            session_file: Some(path),
            header,
            entries,
            leaf_id,
            persist: true,
            flushed: true,
        };
        if original_entries != manager.serialized_entries() {
            manager.rewrite_file()?;
        }
        Ok(manager)
    }

    pub fn continue_recent(
        cwd: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
    ) -> Result<Self, SessionManagerError> {
        let cwd = cwd.into();
        let session_dir = session_dir.unwrap_or_else(|| default_session_dir(&cwd));
        fs::create_dir_all(&session_dir)?;

        let most_recent = fs::read_dir(&session_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                    return None;
                }
                if !has_valid_header(&path) {
                    return None;
                }
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((path, modified))
            })
            .max_by(|left, right| left.1.cmp(&right.1));

        match most_recent {
            Some((path, _)) => Self::open(path),
            None => Self::create(cwd, Some(session_dir)),
        }
    }

    pub fn fork_from(
        source_path: impl Into<PathBuf>,
        target_cwd: impl Into<PathBuf>,
        session_dir: Option<PathBuf>,
    ) -> Result<Self, SessionManagerError> {
        let source_path = source_path.into();
        let target_cwd = target_cwd.into();
        let session_dir = session_dir.unwrap_or_else(|| default_session_dir(&target_cwd));
        fs::create_dir_all(&session_dir)?;

        let content = fs::read_to_string(&source_path)?;
        if !content_has_valid_header(&content) {
            return Err(SessionManagerError::InvalidSessionFile);
        }

        let mut raw_entries = parse_session_entries(&content);
        migrate_session_entries(&mut raw_entries);
        let entries = raw_entries
            .into_iter()
            .filter(|entry| parse_header(entry).ok().flatten().is_none())
            .collect::<Vec<_>>();

        let timestamp = now_iso_timestamp();
        let session_id = Uuid::new_v4().to_string();
        let file_timestamp = timestamp.replace(':', "-").replace('.', "-");
        let session_file = session_dir.join(format!("{file_timestamp}_{session_id}.jsonl"));
        let header = SessionHeader {
            entry_type: "session".to_string(),
            version: Some(CURRENT_SESSION_VERSION),
            id: session_id,
            timestamp,
            cwd: target_cwd.to_string_lossy().to_string(),
            parent_session: Some(source_path.to_string_lossy().to_string()),
        };

        let manager = Self {
            cwd: target_cwd,
            session_dir,
            session_file: Some(session_file),
            header,
            leaf_id: last_entry_id(&entries),
            entries,
            persist: true,
            flushed: true,
        };
        manager.rewrite_file()?;
        Ok(manager)
    }

    pub fn get_header(&self) -> &SessionHeader {
        &self.header
    }

    pub fn get_session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    pub fn get_session_id(&self) -> &str {
        &self.header.id
    }

    pub fn get_session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn get_cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn get_entries(&self) -> &[Value] {
        &self.entries
    }

    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn get_entry(&self, id: &str) -> Option<&Value> {
        self.entries.iter().find(|entry| {
            parse_entry_base(entry)
                .ok()
                .flatten()
                .is_some_and(|base| base.id == id)
        })
    }

    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<Value> {
        let mut by_id = HashMap::new();
        for entry in &self.entries {
            if let Some(base) = parse_entry_base(entry).ok().flatten() {
                by_id.insert(base.id, entry.clone());
            }
        }

        let mut branch = Vec::new();
        let mut current_id = from_id
            .map(ToOwned::to_owned)
            .or_else(|| self.leaf_id.clone());
        while let Some(id) = current_id {
            let Some(entry) = by_id.get(&id) else {
                break;
            };
            branch.push(entry.clone());
            current_id = parse_entry_base(entry)
                .ok()
                .flatten()
                .and_then(|base| base.parent_id);
        }
        branch.reverse();
        branch
    }

    pub fn get_tree(&self) -> Vec<crate::SessionTreeNode> {
        build_session_tree(&self.entries)
    }

    pub fn append_message(&mut self, message: Message) -> Result<String, SessionManagerError> {
        let entry = SessionMessageEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            message,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::Message(entry))?)?;
        Ok(id)
    }

    pub fn append_model_change(
        &mut self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionModelChangeEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            provider: provider.into(),
            model_id: model_id.into(),
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::ModelChange(entry))?)?;
        Ok(id)
    }

    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: impl Into<String>,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionThinkingLevelChangeEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            thinking_level: thinking_level.into(),
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::ThinkingLevelChange(
            entry,
        ))?)?;
        Ok(id)
    }

    pub fn append_compaction(
        &mut self,
        summary: impl Into<String>,
        first_kept_entry_id: impl Into<String>,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionManagerError> {
        let first_kept_entry_id = first_kept_entry_id.into();
        if self.get_entry(&first_kept_entry_id).is_none() {
            return Err(SessionManagerError::MissingEntry(first_kept_entry_id));
        }

        let entry = SessionCompactionEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            summary: summary.into(),
            first_kept_entry_id,
            tokens_before,
            details,
            from_hook,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::Compaction(entry))?)?;
        Ok(id)
    }

    pub fn append_custom_entry(
        &mut self,
        custom_type: impl Into<String>,
        data: Option<Value>,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionCustomEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            custom_type: custom_type.into(),
            data,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::Custom(entry))?)?;
        Ok(id)
    }

    pub fn append_custom_message_entry(
        &mut self,
        custom_type: impl Into<String>,
        content: UserContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionManagerError> {
        let entry = SessionCustomMessageEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            custom_type: custom_type.into(),
            content,
            display,
            details,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::CustomMessage(entry))?)?;
        Ok(id)
    }

    pub fn append_label_change(
        &mut self,
        target_id: impl Into<String>,
        label: Option<String>,
    ) -> Result<String, SessionManagerError> {
        let target_id = target_id.into();
        if self.get_entry(&target_id).is_none() {
            return Err(SessionManagerError::MissingEntry(target_id));
        }

        let entry = SessionLabelEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            target_id,
            label,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::Label(entry))?)?;
        Ok(id)
    }

    pub fn append_session_info(
        &mut self,
        name: impl Into<String>,
    ) -> Result<String, SessionManagerError> {
        let trimmed = name.into().trim().to_string();
        let entry = SessionInfoEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            name: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            },
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::SessionInfo(entry))?)?;
        Ok(id)
    }

    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionManagerError> {
        if self.get_entry(branch_from_id).is_none() {
            return Err(SessionManagerError::MissingEntry(
                branch_from_id.to_string(),
            ));
        }
        self.leaf_id = Some(branch_from_id.to_string());
        Ok(())
    }

    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: impl Into<String>,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionManagerError> {
        if let Some(branch_from_id) = branch_from_id {
            self.branch(branch_from_id)?;
        } else {
            self.reset_leaf();
        }

        let entry = SessionBranchSummaryEntry {
            id: short_uuid(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso_timestamp(),
            from_id: branch_from_id.unwrap_or("root").to_string(),
            summary: summary.into(),
            details,
            from_hook,
        };
        let id = entry.id.clone();
        self.append_serialized_entry(serde_json::to_value(&SessionEntry::BranchSummary(entry))?)?;
        Ok(id)
    }

    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<PathBuf>, SessionManagerError> {
        let branch_entries = self.get_branch(Some(leaf_id));
        if branch_entries.is_empty() {
            return Err(SessionManagerError::MissingEntry(leaf_id.to_string()));
        }

        let path_without_labels = branch_entries
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("label"))
            .cloned()
            .collect::<Vec<_>>();
        let path_entry_ids = path_without_labels
            .iter()
            .filter_map(|entry| parse_entry_base(entry).ok().flatten().map(|base| base.id))
            .collect::<Vec<_>>();
        let labels_by_target = self.current_labels();

        let timestamp = now_iso_timestamp();
        let new_header = SessionHeader {
            entry_type: "session".to_string(),
            version: Some(CURRENT_SESSION_VERSION),
            id: Uuid::new_v4().to_string(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.to_string_lossy().to_string(),
            parent_session: self
                .session_file
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
        };

        let mut new_entries = path_without_labels;
        let mut parent_id = new_entries
            .last()
            .and_then(|entry| parse_entry_base(entry).ok().flatten().map(|base| base.id));
        for target_id in path_entry_ids {
            let Some(label) = labels_by_target.get(&target_id).cloned() else {
                continue;
            };
            let entry = SessionLabelEntry {
                id: short_uuid(),
                parent_id: parent_id.clone(),
                timestamp: now_iso_timestamp(),
                target_id,
                label: Some(label),
            };
            parent_id = Some(entry.id.clone());
            new_entries.push(serde_json::to_value(SessionEntry::Label(entry))?);
        }

        let new_session_file = if self.persist {
            let file_timestamp = timestamp.replace(':', "-").replace('.', "-");
            Some(
                self.session_dir
                    .join(format!("{file_timestamp}_{}.jsonl", new_header.id)),
            )
        } else {
            None
        };

        self.header = new_header;
        self.session_file = new_session_file.clone();
        self.entries = new_entries;
        self.leaf_id = last_entry_id(&self.entries);
        self.flushed = true;

        if self.persist {
            self.rewrite_file()?;
        }

        Ok(new_session_file)
    }

    pub fn get_session_name(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| {
            serde_json::from_value::<SessionEntry>(entry.clone())
                .ok()
                .and_then(|entry| match entry {
                    SessionEntry::SessionInfo(entry) => entry.name,
                    _ => None,
                })
        })
    }

    pub fn build_session_context(&self) -> RuntimeSessionContext {
        if self.leaf_id.is_none() {
            return RuntimeSessionContext {
                messages: Vec::new(),
                thinking_level: Some("off".to_string()),
                model: None,
            };
        }

        let branch = self.get_branch(None);
        let mut thinking_level = Some("off".to_string());
        let mut model = None;
        let mut compaction = None;
        for entry in branch {
            if let Ok(SessionEntry::Message(message_entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                if let Message::Assistant(assistant) = &message_entry.message {
                    model = Some((assistant.provider.0.clone(), assistant.model.clone()));
                }
                continue;
            }
            if let Ok(SessionEntry::ThinkingLevelChange(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                thinking_level = Some(entry.thinking_level);
                continue;
            }
            if let Ok(SessionEntry::ModelChange(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                model = Some((entry.provider, entry.model_id));
                continue;
            }
            if let Ok(SessionEntry::Compaction(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                compaction = Some(entry);
                continue;
            }
        }

        let path = self.get_branch(None);
        let mut contextual_messages = Vec::new();
        let append_context_message = |entry: &Value, messages: &mut Vec<Message>| {
            if let Ok(SessionEntry::Message(message_entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                messages.push(message_entry.message);
                return;
            }
            if let Ok(SessionEntry::CustomMessage(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                messages.push(Message::User(UserMessage {
                    content: entry.content,
                    timestamp: timestamp_to_millis(&entry.timestamp),
                }));
                return;
            }
            if let Ok(SessionEntry::BranchSummary(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                if entry.summary.is_empty() {
                    return;
                }
                messages.push(summary_message(
                    format!(
                        "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                        entry.summary
                    ),
                    &entry.timestamp,
                ));
            }
        };

        if let Some(compaction) = compaction {
            contextual_messages.push(summary_message(
                format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    compaction.summary
                ),
                &compaction.timestamp,
            ));
            let compaction_index = path.iter().position(|entry| {
                parse_entry_base(entry)
                    .ok()
                    .flatten()
                    .is_some_and(|base| base.id == compaction.id)
            });
            if let Some(compaction_index) = compaction_index {
                let mut found_first_kept = false;
                for entry in path.iter().take(compaction_index) {
                    if parse_entry_base(entry)
                        .ok()
                        .flatten()
                        .is_some_and(|base| base.id == compaction.first_kept_entry_id)
                    {
                        found_first_kept = true;
                    }
                    if found_first_kept {
                        append_context_message(entry, &mut contextual_messages);
                    }
                }
                for entry in path.iter().skip(compaction_index + 1) {
                    append_context_message(entry, &mut contextual_messages);
                }
            }
        } else {
            for entry in &path {
                append_context_message(entry, &mut contextual_messages);
            }
        }

        RuntimeSessionContext {
            messages: contextual_messages,
            thinking_level,
            model,
        }
    }

    pub fn context(
        &self,
        system_prompt: Option<String>,
        tools: Option<Vec<cell_ai_core::ToolDefinition>>,
    ) -> Context {
        let session_context = self.build_session_context();
        Context {
            system_prompt,
            messages: session_context.messages,
            tools,
        }
    }

    fn append_serialized_entry(&mut self, entry: Value) -> Result<(), SessionManagerError> {
        if let Some(base) = parse_entry_base(&entry).ok().flatten() {
            self.leaf_id = Some(base.id);
        }
        self.entries.push(entry);
        self.persist_latest_entry()?;
        Ok(())
    }

    fn persist_latest_entry(&mut self) -> Result<(), SessionManagerError> {
        if !self.persist || !self.has_assistant_message() {
            self.flushed = false;
            return Ok(());
        }
        if !self.flushed {
            self.rewrite_file()?;
            self.flushed = true;
            return Ok(());
        }
        if let Some(session_file) = &self.session_file {
            let Some(entry) = self.entries.last() else {
                return Ok(());
            };
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(session_file)?;
            writeln!(file, "{}", serde_json::to_string(entry)?)?;
        }
        Ok(())
    }

    fn rewrite_file(&self) -> Result<(), SessionManagerError> {
        if !self.persist {
            return Ok(());
        }
        if let Some(session_file) = &self.session_file {
            if let Some(parent) = session_file.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(session_file)?;
            for entry in self.serialized_entries() {
                writeln!(file, "{}", serde_json::to_string(&entry)?)?;
            }
        }
        Ok(())
    }

    fn serialized_entries(&self) -> Vec<Value> {
        let mut entries = Vec::with_capacity(self.entries.len() + 1);
        entries.push(serde_json::to_value(&self.header).expect("serialize header"));
        entries.extend(self.entries.clone());
        entries
    }

    fn has_assistant_message(&self) -> bool {
        self.entries.iter().any(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("message")
                && entry
                    .get("message")
                    .and_then(|message| message.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant")
        })
    }

    fn current_labels(&self) -> HashMap<String, String> {
        let mut labels_by_target = HashMap::new();
        for entry in &self.entries {
            if entry.get("type").and_then(Value::as_str) != Some("label") {
                continue;
            }
            let Some(target_id) = entry.get("targetId").and_then(Value::as_str) else {
                continue;
            };
            match entry.get("label").and_then(Value::as_str) {
                Some(label) => {
                    labels_by_target.insert(target_id.to_string(), label.to_string());
                }
                None => {
                    labels_by_target.remove(target_id);
                }
            }
        }
        labels_by_target
    }
}

fn summary_message(text: String, timestamp: &str) -> Message {
    Message::User(UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text {
            text,
            text_signature: None,
        }]),
        timestamp: timestamp_to_millis(timestamp),
    })
}

fn default_session_dir(cwd: &Path) -> PathBuf {
    get_sessions_dir().join(encode_session_dir_name(&cwd.to_string_lossy()))
}

fn last_entry_id(entries: &[Value]) -> Option<String> {
    entries
        .iter()
        .filter_map(|entry| parse_entry_base(entry).ok().flatten())
        .last()
        .map(|entry| entry.id)
}

fn short_uuid() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn now_iso_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn timestamp_to_millis(timestamp: &str) -> i64 {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .map(|value| i64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
        .unwrap_or(0)
}

fn content_has_valid_header(content: &str) -> bool {
    let Some(first_line) = content.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Ok(entry) = serde_json::from_str::<Value>(first_line) else {
        return false;
    };
    parse_header(&entry).ok().flatten().is_some()
}

fn has_valid_header(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    content_has_valid_header(&content)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use cell_ai_core::{
        ApiId, AssistantMessage, Message, ProviderId, StopReason, Usage, UsageCost, UserContent,
        UserContentBlock, UserMessage,
    };
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::SessionManager;

    fn assistant_message(timestamp: i64) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: ApiId::new("openai-responses"),
            provider: ProviderId::new("openai"),
            model: "gpt-5.1-codex".to_string(),
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 2,
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
            timestamp,
        }
    }

    fn message_text(message: &Message) -> String {
        match message {
            Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            }) => text.clone(),
            Message::User(UserMessage {
                content: UserContent::Blocks(blocks),
                ..
            }) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text { text, .. } => Some(text.clone()),
                    UserContentBlock::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Message::Assistant(_) | Message::ToolResult(_) => String::new(),
        }
    }

    #[test]
    fn creates_persistent_session_and_reloads_entries() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::create(tempdir.path(), None).expect("create session");
        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("hello".to_string()),
                timestamp: 0,
            }))
            .expect("append message");
        session
            .append_message(Message::Assistant(assistant_message(1)))
            .expect("append assistant");
        session
            .append_thinking_level_change("high")
            .expect("append thinking");
        session
            .append_model_change("openai", "gpt-5.1-codex")
            .expect("append model");

        let session_file = session
            .get_session_file()
            .expect("session file")
            .to_path_buf();
        let reloaded = SessionManager::open(session_file).expect("open session");
        let context = reloaded.build_session_context();
        assert_eq!(context.messages.len(), 2);
        assert_eq!(context.thinking_level.as_deref(), Some("high"));
        assert_eq!(
            context.model,
            Some(("openai".to_string(), "gpt-5.1-codex".to_string()))
        );
    }

    #[test]
    fn delays_session_file_flush_until_assistant_message() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::create(tempdir.path(), None).expect("create session");
        let session_file = session
            .get_session_file()
            .expect("session file")
            .to_path_buf();

        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("hello".to_string()),
                timestamp: 0,
            }))
            .expect("append user");
        assert!(!session_file.exists());

        session
            .append_message(Message::Assistant(assistant_message(1)))
            .expect("append assistant");
        assert!(session_file.exists());

        let content = fs::read_to_string(session_file).expect("session content");
        assert!(content.contains("\"type\":\"session\""));
        assert!(content.contains("\"role\":\"user\""));
        assert!(content.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn continues_most_recent_session_for_cwd() {
        let tempdir = tempdir().expect("tempdir");
        let session_dir = tempdir.path().join("sessions");

        let mut older =
            SessionManager::create(tempdir.path(), Some(session_dir.clone())).expect("older");
        older
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("first".to_string()),
                timestamp: 1,
            }))
            .expect("append older user");
        older
            .append_message(Message::Assistant(assistant_message(2)))
            .expect("append older assistant");

        std::thread::sleep(Duration::from_millis(5));

        let mut newer =
            SessionManager::create(tempdir.path(), Some(session_dir.clone())).expect("newer");
        newer
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("second".to_string()),
                timestamp: 3,
            }))
            .expect("append newer user");
        newer
            .append_message(Message::Assistant(assistant_message(4)))
            .expect("append newer assistant");

        let continued = SessionManager::continue_recent(tempdir.path(), Some(session_dir))
            .expect("continue recent");
        let branch = continued.get_branch(None);
        let last = branch.last().expect("last branch entry");
        assert_eq!(
            last.get("message")
                .and_then(|message| message.get("timestamp"))
                .and_then(Value::as_i64),
            Some(4)
        );
    }

    #[test]
    fn forks_session_history_into_target_cwd() {
        let source_dir = tempdir().expect("source dir");
        let target_dir = tempdir().expect("target dir");

        let mut source = SessionManager::create(source_dir.path(), None).expect("source session");
        source
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("fork me".to_string()),
                timestamp: 1,
            }))
            .expect("append source user");
        source
            .append_message(Message::Assistant(assistant_message(2)))
            .expect("append source assistant");

        let source_file = source
            .get_session_file()
            .expect("source file")
            .to_path_buf();
        let forked =
            SessionManager::fork_from(&source_file, target_dir.path(), None).expect("forked");
        let header = forked.get_header();
        assert_eq!(header.cwd, target_dir.path().to_string_lossy().to_string());
        assert_eq!(
            header.parent_session.as_deref(),
            Some(source_file.to_string_lossy().as_ref())
        );
        assert_eq!(forked.get_entries().len(), 2);
    }

    #[test]
    fn creates_branched_session_with_selected_path_and_labels() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::create(tempdir.path(), None).expect("create");
        let root_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("root".to_string()),
                timestamp: 1,
            }))
            .expect("root");
        let branch_a = session
            .append_message(Message::Assistant(assistant_message(2)))
            .expect("branch a");
        session
            .append_label_change(&branch_a, Some("keep".to_string()))
            .expect("label");
        session.branch(&root_id).expect("branch root");
        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("branch-b".to_string()),
                timestamp: 3,
            }))
            .expect("branch b");

        let branched_file = session
            .create_branched_session(&branch_a)
            .expect("create branched session")
            .expect("branched file");
        assert!(branched_file.exists());
        assert!(session.get_entries().iter().any(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("label")
                && entry.get("targetId").and_then(Value::as_str) == Some(branch_a.as_str())
        }));
        assert!(!session.get_entries().iter().any(|entry| {
            entry
                .get("message")
                .and_then(|message| message.get("content"))
                .map(|content| content == "branch-b")
                .unwrap_or(false)
        }));
    }

    #[test]
    fn session_info_persists_latest_trimmed_name() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::in_memory(tempdir.path());
        session.append_session_info("  First  ").expect("first");
        session.append_session_info("Second").expect("second");
        assert_eq!(session.get_session_name().as_deref(), Some("Second"));
    }

    #[test]
    fn builds_context_with_branch_summary_and_custom_message_entries() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::in_memory(tempdir.path());
        let root_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("root".to_string()),
                timestamp: 1,
            }))
            .expect("root");
        session
            .branch_with_summary(Some(&root_id), "branch summary", None, None)
            .expect("branch summary");
        session
            .append_custom_message_entry(
                "note",
                UserContent::Text("custom note".to_string()),
                true,
                Some(json!({"kind":"note"})),
            )
            .expect("custom message");

        let context = session.build_session_context();
        assert_eq!(context.messages.len(), 3);
        assert_eq!(message_text(&context.messages[0]), "root");
        assert!(message_text(&context.messages[1]).contains("branch summary"));
        assert_eq!(message_text(&context.messages[2]), "custom note");
    }

    #[test]
    fn hidden_custom_messages_still_participate_in_context_and_empty_branch_summaries_do_not() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::in_memory(tempdir.path());
        let root_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("root".to_string()),
                timestamp: 1,
            }))
            .expect("root");
        session
            .branch_with_summary(Some(&root_id), "", None, None)
            .expect("empty branch summary");
        session
            .append_custom_message_entry(
                "hidden",
                UserContent::Text("hidden note".to_string()),
                false,
                None,
            )
            .expect("hidden custom message");

        let context = session.build_session_context();
        assert_eq!(context.messages.len(), 2);
        assert_eq!(message_text(&context.messages[0]), "root");
        assert_eq!(message_text(&context.messages[1]), "hidden note");
    }

    #[test]
    fn builds_context_with_compaction_summary_and_kept_messages() {
        let tempdir = tempdir().expect("tempdir");
        let mut session = SessionManager::in_memory(tempdir.path());
        session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("discarded".to_string()),
                timestamp: 1,
            }))
            .expect("discarded user");
        session
            .append_message(Message::Assistant(assistant_message(2)))
            .expect("discarded assistant");
        let kept_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("kept".to_string()),
                timestamp: 3,
            }))
            .expect("kept user");
        session
            .append_compaction("compressed", &kept_id, 123, None, None)
            .expect("compaction");
        session
            .append_message(Message::Assistant(assistant_message(4)))
            .expect("post-compaction assistant");

        let context = session.build_session_context();
        assert_eq!(context.messages.len(), 3);
        assert!(message_text(&context.messages[0]).contains("compressed"));
        assert_eq!(message_text(&context.messages[1]), "kept");
        match &context.messages[2] {
            Message::Assistant(assistant) => assert_eq!(assistant.timestamp, 4),
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
