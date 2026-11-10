//! Provider fallback chains (T6a): a model call that fails in a way another
//! provider might not (overload, 5xx, rate limits, network, auth or billing)
//! moves to the next model in the chain once its own retries are spent.
//!
//! The chain answers like any provider. When a later model answers, its
//! stream starts with [`StreamEvent::Fallback`], which the agent records on
//! the journaled turn, so resume and replay reuse that result and never
//! re-pick. Errors that would be wrong anywhere (other 4xx, a bad request)
//! are returned as they are, without trying the rest.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use tern_core::config::ModelRef;
use tern_core::types::{ChatRequest, EventStream, Fallback, Provider, ProviderError, StreamEvent};
use tokio_util::sync::CancellationToken;

/// One model in a chain and the provider that serves it.
pub struct Member {
    pub model: ModelRef,
    pub provider: Arc<dyn Provider>,
}

pub struct ChainProvider {
    members: Vec<Member>,
    /// Attempts per model before moving on, retries included.
    attempts: u32,
    retry_base: Duration,
}

impl ChainProvider {
    /// `members` in order, the run's own model first. Each model gets the
    /// agent's usual retries before the next is tried.
    pub fn new(members: Vec<Member>) -> Self {
        assert!(!members.is_empty(), "a chain needs a model");
        Self {
            members,
            attempts: tern_core::agent::MAX_LLM_ATTEMPTS,
            retry_base: Duration::from_secs(1),
        }
    }

    pub fn with_retries(mut self, attempts: u32, base: Duration) -> Self {
        self.attempts = attempts.max(1);
        self.retry_base = base;
        self
    }

    /// Open `member`'s stream and wait for its first event, so an error the
    /// provider reports at the start of a 200 stream (Anthropic's
    /// `overloaded_error`) counts as a failed call, not a failed answer.
    async fn open(
        member: &Member,
        mut req: ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        req.model = member.model.model.clone();
        let mut stream = member.provider.stream(req, cancel.clone()).await?;
        let first = tokio::select! {
            n = stream.next() => n,
            () = cancel.cancelled() => return Err(ProviderError::Cancelled),
        };
        match first {
            Some(Err(e)) => Err(e),
            Some(Ok(ev)) => Ok(Box::pin(futures::stream::iter([Ok(ev)]).chain(stream))),
            None => Ok(stream),
        }
    }
}

/// A short reason for passing a model over, e.g. `HTTP 529 (overloaded)`.
pub fn reason(e: &ProviderError) -> String {
    match e {
        ProviderError::Http { status, .. } => {
            let what = match status {
                529 => "overloaded",
                429 => "rate limited",
                401 | 403 => "auth failed",
                402 => "out of credit",
                408 => "timed out",
                s if *s >= 500 => "server error",
                _ => "rejected",
            };
            format!("HTTP {status} ({what})")
        }
        ProviderError::Network(_) => "network error".into(),
        ProviderError::Stream { message, .. } => {
            let mut m = message.clone();
            if m.len() > 120 {
                let cut = (0..=120)
                    .rev()
                    .find(|i| m.is_char_boundary(*i))
                    .unwrap_or(0);
                m.truncate(cut);
                m.push('…');
            }
            m
        }
        other => other.to_string(),
    }
}

#[async_trait]
impl Provider for ChainProvider {
    fn id(&self) -> &str {
        self.members[0].provider.id()
    }

    /// The smallest every model allows, so a fallback isn't asked for more
    /// than it can give. Unknown for any model = the agent's default.
    fn max_output_tokens(&self) -> Option<u32> {
        self.members
            .iter()
            .map(|m| m.provider.max_output_tokens())
            .collect::<Option<Vec<u32>>>()
            .and_then(|v| v.into_iter().min())
    }

    /// The primary's: it is the one whose cache the fork reads.
    fn fork_min_tokens(&self, model: &str) -> Option<u64> {
        self.members[0].provider.fork_min_tokens(model)
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let mut passed: Vec<String> = Vec::new();
        for (i, member) in self.members.iter().enumerate() {
            let mut attempt = 1;
            let err = loop {
                let e = match Self::open(member, req.clone(), &cancel).await {
                    Ok(stream) if i == 0 => return Ok(stream),
                    Ok(stream) => {
                        let fallback = Fallback {
                            model: member.model.to_string(),
                            reason: passed.join("; "),
                        };
                        tracing::warn!(model = %fallback.model, reason = %fallback.reason, "answered by a fallback model");
                        let first = futures::stream::iter([Ok(StreamEvent::Fallback(fallback))]);
                        return Ok(Box::pin(first.chain(stream)));
                    }
                    Err(e) => e,
                };
                if !e.moves_to_fallback() {
                    return Err(e);
                }
                if !e.is_retryable() || attempt >= self.attempts {
                    break e;
                }
                let delay = e
                    .retry_after()
                    .unwrap_or_else(|| tern_core::agent::backoff(self.retry_base, attempt));
                tracing::warn!(model = %member.model, attempt, ?delay, error = %e, "model call failed; retrying");
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = cancel.cancelled() => return Err(ProviderError::Cancelled),
                }
                attempt += 1;
            };
            tracing::warn!(model = %member.model, error = %err, "model call failed; trying the next in the chain");
            passed.push(format!("{} {}", member.model, reason(&err)));
        }
        // Not retryable: each model already had its retries, and the agent
        // would only go round the chain again.
        Err(ProviderError::Stream {
            message: format!("every model in the chain failed: {}", passed.join("; ")),
            retryable: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tern_core::types::{Message, StopReason, Usage};

    use super::*;

    /// Fails with `errors` in turn, then answers with its model name.
    struct Flaky {
        errors: Mutex<Vec<ProviderError>>,
        calls: Mutex<Vec<String>>,
        max_out: Option<u32>,
    }

    impl Flaky {
        fn new(errors: Vec<ProviderError>) -> Arc<Self> {
            Arc::new(Self {
                errors: Mutex::new(errors),
                calls: Mutex::new(Vec::new()),
                max_out: None,
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Provider for Flaky {
        fn id(&self) -> &str {
            "flaky"
        }
        fn max_output_tokens(&self) -> Option<u32> {
            self.max_out
        }
        async fn stream(
            &self,
            req: ChatRequest,
            _cancel: CancellationToken,
        ) -> Result<EventStream, ProviderError> {
            self.calls.lock().unwrap().push(req.model.clone());
            let mut errors = self.errors.lock().unwrap();
            if !errors.is_empty() {
                let e = errors.remove(0);
                // Overload as the first event of a 200 stream.
                if matches!(e, ProviderError::Stream { .. }) {
                    return Ok(Box::pin(futures::stream::iter([Err(e)])));
                }
                return Err(e);
            }
            Ok(Box::pin(futures::stream::iter([
                Ok(StreamEvent::TextDelta(req.model)),
                Ok(StreamEvent::MessageStop {
                    usage: Usage::default(),
                    stop_reason: StopReason::EndTurn,
                }),
            ])))
        }
    }

    fn http(status: u16) -> ProviderError {
        ProviderError::Http {
            status,
            body: "nope".into(),
            retry_after: None,
        }
    }

    fn chain(members: &[(&str, &Arc<Flaky>)]) -> ChainProvider {
        ChainProvider::new(
            members
                .iter()
                .map(|(m, p)| Member {
                    model: m.parse().unwrap(),
                    provider: (*p).clone(),
                })
                .collect(),
        )
        .with_retries(2, Duration::from_millis(1))
    }

    fn req() -> ChatRequest {
        ChatRequest {
            model: "x".into(),
            system: String::new(),
            messages: vec![Message::user_text("hi")],
            tools: vec![],
            max_tokens: 100,
            forbid_tool_calls: false,
            cache: Default::default(),
        }
    }

    async fn events(p: &ChainProvider) -> Result<Vec<StreamEvent>, ProviderError> {
        let s = p.stream(req(), CancellationToken::new()).await?;
        s.collect::<Vec<_>>().await.into_iter().collect()
    }

    #[tokio::test]
    async fn primary_answers_without_a_marker() {
        let a = Flaky::new(vec![http(529)]);
        let b = Flaky::new(vec![]);
        let evs = events(&chain(&[("a:one", &a), ("b:two", &b)]))
            .await
            .unwrap();
        // One retry on the primary was enough.
        assert_eq!(a.calls(), ["one", "one"]);
        assert!(b.calls().is_empty());
        assert_eq!(evs[0], StreamEvent::TextDelta("one".into()));
    }

    #[tokio::test]
    async fn overload_moves_to_the_next_model_after_retries() {
        let overloaded = || ProviderError::Stream {
            message: "overloaded_error: Overloaded".into(),
            retryable: true,
        };
        let a = Flaky::new(vec![overloaded(), overloaded()]);
        let b = Flaky::new(vec![]);
        let evs = events(&chain(&[("a:one", &a), ("b:two", &b)]))
            .await
            .unwrap();
        assert_eq!(a.calls(), ["one", "one"]);
        assert_eq!(b.calls(), ["two"]);
        let StreamEvent::Fallback(f) = &evs[0] else {
            panic!("no fallback marker: {evs:?}");
        };
        assert_eq!(f.model, "b:two");
        assert_eq!(f.reason, "a:one overloaded_error: Overloaded");
        assert_eq!(evs[1], StreamEvent::TextDelta("two".into()));
    }

    #[tokio::test]
    async fn auth_failure_moves_on_without_retrying() {
        let a = Flaky::new(vec![http(401)]);
        let b = Flaky::new(vec![http(503), http(503)]);
        let c = Flaky::new(vec![]);
        let evs = events(&chain(&[("a:one", &a), ("b:two", &b), ("c:three", &c)]))
            .await
            .unwrap();
        assert_eq!(a.calls(), ["one"]);
        assert_eq!(b.calls(), ["two", "two"]);
        let StreamEvent::Fallback(f) = &evs[0] else {
            panic!("no fallback marker: {evs:?}");
        };
        assert_eq!(f.model, "c:three");
        assert_eq!(
            f.reason,
            "a:one HTTP 401 (auth failed); b:two HTTP 503 (server error)"
        );
    }

    #[tokio::test]
    async fn bad_request_does_not_fall_back() {
        let a = Flaky::new(vec![http(400)]);
        let b = Flaky::new(vec![]);
        let err = events(&chain(&[("a:one", &a), ("b:two", &b)]))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Http { status: 400, .. }),
            "{err}"
        );
        assert_eq!(a.calls(), ["one"]);
        assert!(b.calls().is_empty());
    }

    #[tokio::test]
    async fn exhausted_chain_is_not_retried_again() {
        let a = Flaky::new(vec![http(529), http(529)]);
        let b = Flaky::new(vec![ProviderError::Network("reset".into()); 2]);
        let err = events(&chain(&[("a:one", &a), ("b:two", &b)]))
            .await
            .unwrap_err();
        assert!(!err.is_retryable());
        assert!(
            err.to_string()
                .contains("a:one HTTP 529 (overloaded); b:two network error"),
            "{err}"
        );
    }

    #[test]
    fn output_cap_is_the_smallest_known() {
        let mut a = Flaky::new(vec![]);
        Arc::get_mut(&mut a).unwrap().max_out = Some(64_000);
        let mut b = Flaky::new(vec![]);
        Arc::get_mut(&mut b).unwrap().max_out = Some(8_000);
        let c = Flaky::new(vec![]);
        assert_eq!(
            chain(&[("a:x", &a), ("b:y", &b)]).max_output_tokens(),
            Some(8_000)
        );
        assert_eq!(chain(&[("a:x", &a), ("c:z", &c)]).max_output_tokens(), None);
    }
}
