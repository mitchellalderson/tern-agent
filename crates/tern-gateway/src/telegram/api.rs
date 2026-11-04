//! The handful of Telegram Bot API methods tern needs, over our own reqwest
//! client (teloxide would bring a second reqwest and TLS stack).

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_API_URL: &str = "https://api.telegram.org";
/// Telegram's per-message limit, in UTF-16 code units; we count chars,
/// which is conservative for anything outside the BMP only by a little.
pub const MAX_MESSAGE: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Http(String),
    /// Telegram said `ok: false`.
    Telegram {
        code: i64,
        description: String,
        retry_after: Option<u64>,
    },
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Http(e) => write!(f, "telegram request failed: {e}"),
            ApiError::Telegram {
                code, description, ..
            } => write!(f, "telegram error {code}: {description}"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub from: Option<User>,
    pub text: Option<String>,
    /// In a forum supergroup, or a private chat with topics (Bot API 9.3):
    /// the topic it was posted in.
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    /// Posted in a topic (private chats with topics, Bot API 9.3).
    #[serde(default)]
    pub is_topic_message: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: String,
    /// A supergroup with topics enabled.
    #[serde(default)]
    pub is_forum: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub data: Option<String>,
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
struct Envelope {
    ok: bool,
    result: Option<Value>,
    error_code: Option<i64>,
    description: Option<String>,
    parameters: Option<Parameters>,
}

#[derive(Debug, Clone, Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}

#[derive(Clone)]
pub struct Api {
    base: String,
    client: reqwest::Client,
}

/// One row of inline-keyboard buttons as (label, callback data).
pub type Buttons = Vec<(String, String)>;

impl Api {
    pub fn new(api_url: &str, token: &str) -> Result<Self, reqwest::Error> {
        let client = tern_core::net::http_client()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self {
            base: format!("{}/bot{token}", api_url.trim_end_matches('/')),
            client,
        })
    }

    /// Call a method, retrying once if Telegram asks us to slow down.
    pub async fn call(&self, method: &str, body: Value) -> Result<Value, ApiError> {
        for attempt in 0..2 {
            let resp = self
                .client
                .post(format!("{}/{method}", self.base))
                .json(&body)
                .send()
                .await
                // The URL contains the bot token; never include it in errors.
                .map_err(|e| ApiError::Http(e.without_url().to_string()))?;
            let env: Envelope = resp
                .json()
                .await
                .map_err(|e| ApiError::Http(e.without_url().to_string()))?;
            if env.ok {
                return Ok(env.result.unwrap_or(Value::Null));
            }
            let retry_after = env.parameters.and_then(|p| p.retry_after);
            if let (Some(secs), 0) = (retry_after, attempt) {
                tokio::time::sleep(Duration::from_secs(secs.min(30))).await;
                continue;
            }
            return Err(ApiError::Telegram {
                code: env.error_code.unwrap_or(0),
                description: env.description.unwrap_or_default(),
                retry_after,
            });
        }
        Err(ApiError::Http("rate limited".into()))
    }

    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
    ) -> Result<Vec<Update>, ApiError> {
        let v = self
            .call(
                "getUpdates",
                json!({ "offset": offset, "timeout": timeout_secs, "allowed_updates": ["message", "callback_query"] }),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| ApiError::Http(format!("decoding updates: {e}")))
    }

    fn markup(buttons: Option<&Buttons>) -> Value {
        match buttons {
            None => Value::Null,
            Some(row) => json!({
                "inline_keyboard": [row.iter().map(|(t, d)| json!({ "text": t, "callback_data": d })).collect::<Vec<_>>()]
            }),
        }
    }

    /// Send plain text (no parse mode, so model output needs no escaping).
    pub async fn send_message(
        &self,
        chat: i64,
        text: &str,
        buttons: Option<&Buttons>,
    ) -> Result<i64, ApiError> {
        self.send_message_in(chat, None, text, buttons).await
    }

    /// [`Api::send_message`] into a forum topic (`thread`).
    pub async fn send_message_in(
        &self,
        chat: i64,
        thread: Option<i64>,
        text: &str,
        buttons: Option<&Buttons>,
    ) -> Result<i64, ApiError> {
        let mut body = json!({ "chat_id": chat, "text": text });
        if let Some(t) = thread {
            body["message_thread_id"] = t.into();
        }
        if buttons.is_some() {
            body["reply_markup"] = Self::markup(buttons);
        }
        let v = self.call("sendMessage", body).await?;
        Ok(v.get("message_id")
            .and_then(Value::as_i64)
            .unwrap_or_default())
    }

    pub async fn edit_message(
        &self,
        chat: i64,
        message_id: i64,
        text: &str,
        buttons: Option<&Buttons>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "chat_id": chat, "message_id": message_id, "text": text });
        body["reply_markup"] = match buttons {
            Some(_) => Self::markup(buttons),
            None => json!({ "inline_keyboard": [] }),
        };
        match self.call("editMessageText", body).await {
            // Editing to identical text is an error in Telegram; it's fine for us.
            Err(ApiError::Telegram { description, .. })
                if description.contains("message is not modified") =>
            {
                Ok(())
            }
            other => other.map(|_| ()),
        }
    }

    pub async fn answer_callback(&self, id: &str, text: &str) -> Result<(), ApiError> {
        self.call(
            "answerCallbackQuery",
            json!({ "callback_query_id": id, "text": text }),
        )
        .await
        .map(|_| ())
    }

    /// Open a topic in a forum supergroup (the bot must be an admin that
    /// may manage topics). Returns its thread id.
    pub async fn create_forum_topic(&self, chat: i64, name: &str) -> Result<i64, ApiError> {
        let v = self
            .call("createForumTopic", json!({ "chat_id": chat, "name": name }))
            .await?;
        v.get("message_thread_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| ApiError::Http("createForumTopic returned no thread id".into()))
    }

    pub async fn typing(&self, chat: i64) {
        let _ = self
            .call(
                "sendChatAction",
                json!({ "chat_id": chat, "action": "typing" }),
            )
            .await;
    }
}

/// Split text into Telegram-sized messages, preferring line breaks.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim_end();
    while rest.chars().count() > max {
        let byte_limit = rest.char_indices().nth(max).map_or(rest.len(), |(i, _)| i);
        let window = &rest[..byte_limit];
        let cut = window
            .rfind('\n')
            .filter(|&i| i > byte_limit / 2)
            .or_else(|| window.rfind(' ').filter(|&i| i > byte_limit / 2))
            .unwrap_or(byte_limit);
        out.push(rest[..cut].trim_end().to_owned());
        rest = rest[cut..].trim_start_matches(['\n', ' ']);
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn splitting() {
        assert_eq!(split_message("short", 10), ["short"]);
        assert_eq!(split_message("", 10), [""]);
        let parts = split_message("line one\nline two\nline three", 12);
        assert_eq!(parts, ["line one", "line two", "line three"]);
        let long = "x".repeat(25);
        let parts = split_message(&long, 10);
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [10, 10, 5]
        );
        for p in split_message(&"word ".repeat(2000), MAX_MESSAGE) {
            assert!(p.chars().count() <= MAX_MESSAGE);
        }
    }

    #[tokio::test]
    async fn methods_and_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botT0K3N/sendMessage"))
            .and(body_partial_json(json!({"chat_id": 7, "text": "hi", "reply_markup": {"inline_keyboard": [[{"text": "Yes", "callback_data": "y"}]]}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": {"message_id": 99}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botT0K3N/editMessageText"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"ok": false, "error_code": 400, "description": "Bad Request: message is not modified"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botT0K3N/getUpdates"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                json!({"ok": false, "error_code": 401, "description": "Unauthorized"}),
            ))
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), "T0K3N").unwrap();
        let buttons = vec![("Yes".to_owned(), "y".to_owned())];
        assert_eq!(api.send_message(7, "hi", Some(&buttons)).await.unwrap(), 99);
        api.edit_message(7, 99, "same", None).await.unwrap();
        let err = api.get_updates(0, 1).await.unwrap_err().to_string();
        assert!(
            err.contains("401") && err.contains("Unauthorized") && !err.contains("T0K3N"),
            "{err}"
        );
    }
}
