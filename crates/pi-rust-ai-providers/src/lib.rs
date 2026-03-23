mod anthropic;
mod common;
mod openai;

use std::collections::HashMap;
use std::sync::Arc;

use pi_rust_ai_core::{AssistantMessageEventStream, Context, Model, StreamOptions};
use thiserror::Error;

pub trait ApiProvider: Send + Sync {
    fn api(&self) -> &'static str;
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn ApiProvider>>,
}

#[derive(Debug, Error)]
pub enum ProviderRegistryError {
    #[error("no API provider registered for api: {0}")]
    MissingProvider(String),
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn ApiProvider>) {
        self.providers.insert(provider.api().to_string(), provider);
    }

    pub fn get(&self, api: &str) -> Option<Arc<dyn ApiProvider>> {
        self.providers.get(api).cloned()
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> Result<AssistantMessageEventStream, ProviderRegistryError> {
        let provider = self
            .get(&model.api.0)
            .ok_or_else(|| ProviderRegistryError::MissingProvider(model.api.0.clone()))?;
        Ok(provider.stream(model, context, options))
    }
}

pub fn register_builtin_providers(registry: &mut ProviderRegistry) {
    registry.register(Arc::new(anthropic::AnthropicMessagesProvider));
    registry.register(Arc::new(openai::OpenAICompletionsProvider));
    registry.register(Arc::new(openai::OpenAIResponsesProvider));
    registry.register(Arc::new(openai::OpenAICodexResponsesProvider));

    anthropic::register_builtin_oauth_provider();
    openai::register_builtin_oauth_provider();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pi_rust_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Model,
        ModelCost, ModelInput, ProviderId, StopReason, StreamOptions, Usage, UsageCost,
    };

    use super::{ApiProvider, ProviderRegistry, register_builtin_providers};

    struct StaticProvider;

    impl ApiProvider for StaticProvider {
        fn api(&self) -> &'static str {
            "mock-api"
        }

        fn stream(
            &self,
            model: &Model,
            _context: &Context,
            _options: Option<StreamOptions>,
        ) -> pi_rust_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = pi_rust_ai_core::AssistantMessageEventStream::new();
            let message = AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: format!("hello from {}", model.id),
                    text_signature: None,
                }],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
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
                timestamp: 0,
            };
            sender.send(AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message,
            });
            stream
        }
    }

    fn mock_model() -> Model {
        Model {
            id: "mock-model".to_string(),
            name: "Mock".to_string(),
            api: ApiId::new("mock-api"),
            provider: ProviderId::new("mock"),
            base_url: "https://example.com".to_string(),
            reasoning: false,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 1,
            max_tokens: 1,
            headers: None,
            compat: None,
        }
    }

    #[tokio::test]
    async fn streams_using_registered_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(StaticProvider));
        let stream = registry
            .stream(
                &mock_model(),
                &Context {
                    system_prompt: None,
                    messages: Vec::new(),
                    tools: None,
                },
                None,
            )
            .expect("stream");
        let result = stream.result().await.expect("result");
        assert_eq!(result.model, "mock-model");
    }

    #[test]
    fn registers_all_builtin_provider_apis() {
        let mut registry = ProviderRegistry::new();
        register_builtin_providers(&mut registry);

        assert!(registry.get("anthropic-messages").is_some());
        assert!(registry.get("openai-completions").is_some());
        assert!(registry.get("openai-responses").is_some());
        assert!(registry.get("openai-codex-responses").is_some());
    }
}
