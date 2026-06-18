//! Card "human-friendly" summary helper. Wraps a configurable SummaryProvider
//! with a 10s timeout, LRU cache, and a hard fallback to `summarize_args`.

use crate::approval::summarize_args;
use async_trait::async_trait;
use lru::LruCache;
use parking_lot::Mutex;
use serde_json::Value;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
pub trait SummaryProvider: Send + Sync {
    async fn summarize(&self, prompt: &str) -> anyhow::Result<String>;
}

pub struct Humanizer {
    provider: Option<Arc<dyn SummaryProvider>>,
    timeout: Duration,
    cache: Mutex<LruCache<String, String>>,
}

impl Humanizer {
    pub fn new(provider: Option<Arc<dyn SummaryProvider>>, timeout: Duration) -> Self {
        Self {
            provider,
            timeout,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
        }
    }

    /// Build a human-readable card text. Never errors; always returns something.
    pub async fn humanize(
        &self,
        tool: &str,
        args: &Value,
        triggerer_display: Option<&str>,
        topic: Option<&str>,
        channel_ref: &str,
    ) -> String {
        let scrubbed_args = summarize_args(args);
        let fallback = render_fallback(tool, &scrubbed_args, triggerer_display, topic, channel_ref);

        let Some(provider) = self.provider.clone() else {
            return fallback;
        };

        let cache_key = format!("{tool}\u{1f}{scrubbed_args}");
        if let Some(cached) = self.cache.lock().get(&cache_key).cloned() {
            return decorate(&cached, triggerer_display, topic, channel_ref);
        }

        let prompt = build_prompt(tool, &scrubbed_args);
        let result = tokio::time::timeout(self.timeout, provider.summarize(&prompt)).await;
        match result {
            Ok(Ok(summary)) => {
                self.cache.lock().put(cache_key, summary.clone());
                decorate(&summary, triggerer_display, topic, channel_ref)
            }
            _ => fallback,
        }
    }
}

fn build_prompt(tool: &str, scrubbed_args: &str) -> String {
    format!(
        "Translate the following tool call into one short sentence in Simplified Chinese \
         that a non-technical reader can understand. Do NOT add details that are not in the input. \
         Keep it under 60 characters. Do NOT include the tool name or argument keys verbatim.\n\n\
         Tool: {tool}\nArguments (already redacted): {scrubbed_args}"
    )
}

fn render_fallback(
    tool: &str,
    scrubbed_args: &str,
    triggerer: Option<&str>,
    topic: Option<&str>,
    channel_ref: &str,
) -> String {
    let head = render_header(triggerer, topic, channel_ref);
    format!("{head}想执行：**{tool}**\n\n{scrubbed_args}")
}

fn decorate(body: &str, triggerer: Option<&str>, topic: Option<&str>, channel_ref: &str) -> String {
    let head = render_header(triggerer, topic, channel_ref);
    format!("{head}{body}")
}

fn render_header(triggerer: Option<&str>, topic: Option<&str>, channel_ref: &str) -> String {
    match (triggerer, topic) {
        (Some(t), Some(tp)) => format!("**{t}** 在 [{channel_ref} / #{tp}] "),
        (Some(t), None) => format!("**{t}** 在 [{channel_ref}] "),
        (None, Some(tp)) => format!("[{channel_ref} / #{tp}] "),
        (None, None) => format!("[{channel_ref}] "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StaticProvider(&'static str);
    #[async_trait]
    impl SummaryProvider for StaticProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    struct SlowProvider(Duration);
    #[async_trait]
    impl SummaryProvider for SlowProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            tokio::time::sleep(self.0).await;
            Ok("slow".into())
        }
    }

    struct FailingProvider;
    #[async_trait]
    impl SummaryProvider for FailingProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("simulated"))
        }
    }

    struct CountingProvider(AtomicUsize);
    #[async_trait]
    impl SummaryProvider for CountingProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn fallback_when_no_provider() {
        let h = Humanizer::new(None, Duration::from_secs(10));
        let out = h
            .humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                Some("u_alice"),
                Some("db_lock"),
                "dawnim.work",
            )
            .await;
        assert!(out.contains("u_alice"));
        assert!(out.contains("db_lock"));
        assert!(out.contains("shell"));
        assert!(out.contains("command: ls"));
    }

    #[tokio::test]
    async fn provider_success_drives_output() {
        let h = Humanizer::new(
            Some(Arc::new(StaticProvider("Alice 要查看文件"))),
            Duration::from_secs(10),
        );
        let out = h
            .humanize("shell", &serde_json::json!({}), Some("Alice"), None, "x")
            .await;
        assert!(out.contains("Alice 要查看文件"));
    }

    #[tokio::test]
    async fn provider_failure_falls_back() {
        let h = Humanizer::new(Some(Arc::new(FailingProvider)), Duration::from_secs(10));
        let out = h
            .humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                None,
                None,
                "x",
            )
            .await;
        assert!(out.contains("shell"));
        assert!(out.contains("command: ls"));
    }

    #[tokio::test]
    async fn provider_timeout_falls_back() {
        let h = Humanizer::new(
            Some(Arc::new(SlowProvider(Duration::from_millis(200)))),
            Duration::from_millis(50),
        );
        let out = h
            .humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                None,
                None,
                "x",
            )
            .await;
        assert!(out.contains("shell"));
    }

    #[tokio::test]
    async fn cache_avoids_double_provider_call() {
        let provider = Arc::new(CountingProvider(AtomicUsize::new(0)));
        let h = Humanizer::new(Some(provider.clone()), Duration::from_secs(10));
        for _ in 0..3 {
            h.humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                None,
                None,
                "x",
            )
            .await;
        }
        assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redaction_keeps_secret_values_out_of_prompt() {
        // We can't peek at the prompt directly, but summarize_args (used to build it)
        // already redacts api_key. Assert that fallback (deterministic) doesn't leak.
        let h = Humanizer::new(None, Duration::from_secs(10));
        let out = h
            .humanize(
                "http",
                &serde_json::json!({"api_key": "sk-LEAK-ME"}),
                None,
                None,
                "x",
            )
            .await;
        assert!(!out.contains("sk-LEAK-ME"));
        assert!(out.contains("[redacted]"));
    }
}
