//! The Telegram Bot API methods the bot uses, over the shared HTTP client.
//!
//! The token is part of every URL, so URLs are never logged and transport
//! errors are logged with `without_url()`.

use crate::security::Secret;
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// Ceiling for a normal Bot API call.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum TgError {
    /// Telegram could not be reached, or the request timed out.
    Network(String),
    /// Telegram answered `ok: false`.
    Api { code: u16, description: String },
    /// The answer was not the Bot API's JSON.
    Decode,
}

impl TgError {
    pub fn is_network(&self) -> bool {
        matches!(self, TgError::Network(_))
    }
    pub fn code(&self) -> Option<u16> {
        match self {
            TgError::Api { code, .. } => Some(*code),
            _ => None,
        }
    }
    /// Telegram refused the Markdown (unbalanced `*`, `_` or backtick).
    pub fn is_parse_error(&self) -> bool {
        matches!(self, TgError::Api { code: 400, description } if description.contains("can't parse entities"))
    }
}

impl std::fmt::Display for TgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TgError::Network(e) => write!(f, "network: {}", e),
            TgError::Api { code, description } => write!(f, "telegram {}: {}", code, description),
            TgError::Decode => write!(f, "unexpected answer"),
        }
    }
}

#[derive(Clone)]
pub struct BotApi {
    http: reqwest::Client,
    base: String,
    token: Secret,
}

impl std::fmt::Debug for BotApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotApi").field("base", &self.base).finish()
    }
}

impl BotApi {
    pub fn new(http: reqwest::Client, base: &str, token: Secret) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base, self.token.expose(), method)
    }

    async fn decode(resp: reqwest::Response) -> Result<Value, TgError> {
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.map_err(|_| TgError::Decode)?;
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(body.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(TgError::Api {
            code: body
                .get("error_code")
                .and_then(Value::as_u64)
                .map(|c| c as u16)
                .unwrap_or(status),
            description: body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Call `method` with a JSON body.
    pub async fn call(&self, method: &str, body: Value) -> Result<Value, TgError> {
        self.call_with_timeout(method, body, CALL_TIMEOUT).await
    }

    pub async fn call_with_timeout(
        &self,
        method: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, TgError> {
        let resp = self
            .http
            .post(self.url(method))
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| TgError::Network(e.without_url().to_string()))?;
        Self::decode(resp).await
    }

    pub async fn get_me(&self) -> Result<Value, TgError> {
        self.call_with_timeout("getMe", json!({}), Duration::from_secs(10))
            .await
    }

    /// Long poll. The request outlives the poll by a margin so a quiet poll
    /// is never mistaken for a network failure.
    pub async fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Value>, TgError> {
        let r = self
            .call_with_timeout(
                "getUpdates",
                json!({
                    "offset": offset,
                    "timeout": timeout_secs,
                    "allowed_updates": ["message", "callback_query"],
                }),
                Duration::from_secs(timeout_secs + 15),
            )
            .await?;
        Ok(r.as_array().cloned().unwrap_or_default())
    }

    /// Polling and a webhook cannot coexist; dropping the backlog matches the
    /// web's `drop_pending_updates=True`.
    pub async fn delete_webhook(&self) -> Result<Value, TgError> {
        self.call("deleteWebhook", json!({"drop_pending_updates": true}))
            .await
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        markdown: bool,
        reply_markup: Option<Value>,
    ) -> Result<Value, TgError> {
        let mut body = json!({"chat_id": chat_id, "text": text});
        if markdown {
            body["parse_mode"] = json!("Markdown");
        }
        if let Some(m) = reply_markup {
            body["reply_markup"] = m;
        }
        self.call("sendMessage", body).await
    }

    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        markdown: bool,
        reply_markup: Option<Value>,
    ) -> Result<Value, TgError> {
        let mut body = json!({"chat_id": chat_id, "message_id": message_id, "text": text});
        if markdown {
            body["parse_mode"] = json!("Markdown");
        }
        if let Some(m) = reply_markup {
            body["reply_markup"] = m;
        }
        self.call("editMessageText", body).await
    }

    pub async fn answer_callback_query(&self, id: &str) -> Result<Value, TgError> {
        self.call("answerCallbackQuery", json!({"callback_query_id": id}))
            .await
    }

    pub async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<Value, TgError> {
        self.call(
            "deleteMessage",
            json!({"chat_id": chat_id, "message_id": message_id}),
        )
        .await
    }

    async fn send_form(&self, method: &str, form: Form) -> Result<Value, TgError> {
        let resp = self
            .http
            .post(self.url(method))
            .timeout(Duration::from_secs(60))
            .multipart(form)
            .send()
            .await
            .map_err(|e| TgError::Network(e.without_url().to_string()))?;
        Self::decode(resp).await
    }

    fn png_part(png: Vec<u8>, name: &str) -> Result<Part, TgError> {
        Part::bytes(png)
            .file_name(format!("{}.png", name))
            .mime_str("image/png")
            .map_err(|_| TgError::Decode)
    }

    pub async fn send_photo(
        &self,
        chat_id: i64,
        png: Vec<u8>,
        caption: &str,
    ) -> Result<Value, TgError> {
        let form = Form::new()
            .text("chat_id", chat_id.to_string())
            .text("caption", caption.to_string())
            .part("photo", Self::png_part(png, "chart")?);
        self.send_form("sendPhoto", form).await
    }

    pub async fn send_media_group(
        &self,
        chat_id: i64,
        photos: Vec<(Vec<u8>, String)>,
    ) -> Result<Value, TgError> {
        let mut media = Vec::new();
        let mut form = Form::new().text("chat_id", chat_id.to_string());
        for (i, (png, caption)) in photos.into_iter().enumerate() {
            let name = format!("photo{}", i);
            media.push(
                json!({"type": "photo", "media": format!("attach://{}", name), "caption": caption}),
            );
            form = form.part(name.clone(), Self::png_part(png, &name)?);
        }
        form = form.text("media", Value::Array(media).to_string());
        self.send_form("sendMediaGroup", form).await
    }
}
