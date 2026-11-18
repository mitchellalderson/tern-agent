//! Desktop notifications as a test sees them: the `notification` events a
//! daemon sends on `/v1/events` when a routine's result is pushed (a
//! stand-in for the app that raises the macOS notification).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;

#[derive(Clone, Default)]
pub struct Notes {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Notes {
    pub fn start() -> Self {
        Self::default()
    }

    /// Follow the daemon on `socket`, reconnecting across restarts.
    pub fn watch(&self, socket: PathBuf) {
        let seen = self.seen.clone();
        tokio::spawn(async move {
            loop {
                if socket.exists()
                    && let Ok(http) = reqwest::Client::builder()
                        .unix_socket(socket.clone())
                        .build()
                    && let Ok(resp) = http.get("http://tern/v1/events").send().await
                {
                    let mut buf = String::new();
                    let mut body = resp.bytes_stream();
                    while let Some(Ok(chunk)) = body.next().await {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        while let Some(i) = buf.find('\n') {
                            let line: String = buf.drain(..=i).collect();
                            let Some(data) = line.trim().strip_prefix("data:") else {
                                continue;
                            };
                            if let Ok(v) = serde_json::from_str::<Value>(data.trim())
                                && let Some(n) = v.get("notification")
                                && let Some(text) = n["body"].as_str()
                            {
                                seen.lock().unwrap().push(text.to_owned());
                            }
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }

    pub async fn texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    pub async fn count(&self, needle: &str) -> usize {
        self.texts()
            .await
            .iter()
            .filter(|t| t.contains(needle))
            .count()
    }

    /// Wait for a notification containing `needle`; returns its text.
    pub async fn wait_for(&self, needle: &str, secs: u64) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(t) = self.texts().await.into_iter().find(|t| t.contains(needle)) {
                return t;
            }
            assert!(
                Instant::now() < end,
                "no notification containing {needle:?}; got {:#?}",
                self.texts().await
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
