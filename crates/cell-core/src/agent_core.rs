use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use cell_ai_core::{AssistantMessageEvent, Message, Model};
use cell_protocol::QueueMode;
use serde_json::Value;
use tokio::sync::Notify;

pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Clone, Debug)]
pub struct AgentState {
    pub system_prompt: Option<String>,
    pub model: Model,
    pub thinking_level: String,
    pub messages: Vec<Message>,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub auto_compaction_enabled: bool,
    pub auto_retry_enabled: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Agent {
    state: AgentState,
    control: AgentControl,
}

#[derive(Clone, Debug)]
pub struct AgentControl {
    steering_queue: Arc<Mutex<Vec<Message>>>,
    follow_up_queue: Arc<Mutex<Vec<Message>>>,
    abort_requested: Arc<AtomicBool>,
    abort_notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: Message,
        tool_results: Vec<Message>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: Message,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Message,
        is_error: bool,
    },
    AutoCompactionStart {
        reason: String,
    },
    AutoCompactionEnd {
        result: Option<Value>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    },
}

impl Agent {
    pub fn new(
        model: Model,
        thinking_level: impl Into<String>,
        system_prompt: Option<String>,
    ) -> Self {
        Self {
            state: AgentState {
                system_prompt,
                model,
                thinking_level: thinking_level.into(),
                messages: Vec::new(),
                is_streaming: false,
                is_compacting: false,
                steering_mode: QueueMode::OneAtATime,
                follow_up_mode: QueueMode::OneAtATime,
                auto_compaction_enabled: true,
                auto_retry_enabled: false,
                error: None,
            },
            control: AgentControl {
                steering_queue: Arc::new(Mutex::new(Vec::new())),
                follow_up_queue: Arc::new(Mutex::new(Vec::new())),
                abort_requested: Arc::new(AtomicBool::new(false)),
                abort_notify: Arc::new(Notify::new()),
            },
        }
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AgentState {
        &mut self.state
    }

    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.state.messages = messages;
    }

    pub fn set_model(&mut self, model: Model) {
        self.state.model = model;
    }

    pub fn set_thinking_level(&mut self, thinking_level: impl Into<String>) {
        self.state.thinking_level = thinking_level.into();
    }

    pub fn set_system_prompt(&mut self, system_prompt: Option<String>) {
        self.state.system_prompt = system_prompt;
    }

    pub fn set_streaming(&mut self, is_streaming: bool) {
        self.state.is_streaming = is_streaming;
    }

    pub fn set_compacting(&mut self, is_compacting: bool) {
        self.state.is_compacting = is_compacting;
    }

    pub fn set_auto_compaction_enabled(&mut self, enabled: bool) {
        self.state.auto_compaction_enabled = enabled;
    }

    pub fn set_auto_retry_enabled(&mut self, enabled: bool) {
        self.state.auto_retry_enabled = enabled;
    }

    pub fn set_steering_mode(&mut self, mode: QueueMode) {
        self.state.steering_mode = mode;
    }

    pub fn set_follow_up_mode(&mut self, mode: QueueMode) {
        self.state.follow_up_mode = mode;
    }

    pub fn control(&self) -> AgentControl {
        self.control.clone()
    }

    pub fn steer(&mut self, message: Message) {
        self.control.steer(message);
    }

    pub fn follow_up(&mut self, message: Message) {
        self.control.follow_up(message);
    }

    pub fn take_steering_messages(&mut self) -> Vec<Message> {
        match self.state.steering_mode {
            QueueMode::All => self.control.take_all_steering(),
            QueueMode::OneAtATime => self.control.take_steering(1),
        }
    }

    pub fn take_follow_up_messages(&mut self) -> Vec<Message> {
        match self.state.follow_up_mode {
            QueueMode::All => self.control.take_all_follow_up(),
            QueueMode::OneAtATime => self.control.take_follow_up(1),
        }
    }

    pub fn pending_message_count(&self) -> usize {
        self.control.pending_message_count()
    }

    pub fn reset_abort(&self) {
        self.control.reset_abort();
    }

    pub fn abort(&self) {
        self.control.abort();
    }

    pub fn is_abort_requested(&self) -> bool {
        self.control.is_abort_requested()
    }

    pub async fn wait_for_abort(&self) {
        self.control.wait_for_abort().await;
    }
}

pub fn is_valid_thinking_level(value: &str) -> bool {
    THINKING_LEVELS.contains(&value)
}

impl AgentControl {
    pub fn steer(&self, message: Message) {
        self.steering_queue
            .lock()
            .expect("steering queue lock")
            .push(message);
    }

    pub fn follow_up(&self, message: Message) {
        self.follow_up_queue
            .lock()
            .expect("follow-up queue lock")
            .push(message);
    }

    pub fn pending_message_count(&self) -> usize {
        self.steering_queue
            .lock()
            .expect("steering queue lock")
            .len()
            + self
                .follow_up_queue
                .lock()
                .expect("follow-up queue lock")
                .len()
    }

    pub fn pop_last_steering(&self) -> Option<Message> {
        self.steering_queue
            .lock()
            .expect("steering queue lock")
            .pop()
    }

    pub fn pop_last_follow_up(&self) -> Option<Message> {
        self.follow_up_queue
            .lock()
            .expect("follow-up queue lock")
            .pop()
    }

    pub fn abort(&self) {
        self.abort_requested.store(true, Ordering::SeqCst);
        self.abort_notify.notify_waiters();
    }

    pub fn reset_abort(&self) {
        self.abort_requested.store(false, Ordering::SeqCst);
    }

    pub fn is_abort_requested(&self) -> bool {
        self.abort_requested.load(Ordering::SeqCst)
    }

    pub async fn wait_for_abort(&self) {
        loop {
            if self.is_abort_requested() {
                return;
            }
            self.abort_notify.notified().await;
        }
    }

    fn take_all_steering(&self) -> Vec<Message> {
        std::mem::take(&mut *self.steering_queue.lock().expect("steering queue lock"))
    }

    fn take_steering(&self, count: usize) -> Vec<Message> {
        let mut queue = self.steering_queue.lock().expect("steering queue lock");
        let drain_count = count.min(queue.len());
        queue.drain(..drain_count).collect()
    }

    fn take_all_follow_up(&self) -> Vec<Message> {
        std::mem::take(&mut *self.follow_up_queue.lock().expect("follow-up queue lock"))
    }

    fn take_follow_up(&self, count: usize) -> Vec<Message> {
        let mut queue = self.follow_up_queue.lock().expect("follow-up queue lock");
        let drain_count = count.min(queue.len());
        queue.drain(..drain_count).collect()
    }
}
