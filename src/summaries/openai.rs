use std::path::Path;

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, multipart};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;
use tracing::warn;

const FILES_ENDPOINT: &str = "https://api.openai.com/v1/files";
const RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";

#[derive(Clone)]
pub struct OpenAiSummarizer {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiSummarizer {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }

    pub async fn summarize_transcript(
        &self,
        file_path: &Path,
        session_label: &str,
    ) -> Result<String> {
        let file_id = self.upload_transcript(file_path).await?;
        let summary = self.request_summary(&file_id, session_label).await;
        self.cleanup_file(&file_id).await;
        summary
    }

    async fn upload_transcript(&self, file_path: &Path) -> Result<String> {
        let bytes = fs::read(file_path)
            .await
            .with_context(|| format!("reading transcript {}", file_path.display()))?;
        let file_name = file_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("transcript.json");
        let part = multipart::Part::bytes(bytes)
            .file_name(file_name.to_string())
            .mime_str("application/json")
            .context("encoding transcript upload")?;
        let form = multipart::Form::new()
            .text("purpose", "assistants")
            .part("file", part);
        let response = self
            .client
            .post(FILES_ENDPOINT)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("uploading transcript to OpenAI")?
            .error_for_status()
            .context("OpenAI rejected transcript upload")?;

        let body: FileUploadResponse = response
            .json()
            .await
            .context("parsing transcript upload response")?;
        Ok(body.id)
    }

    async fn request_summary(&self, file_id: &str, session_label: &str) -> Result<String> {
        let label = if session_label.trim().is_empty() {
            "Discord session".to_string()
        } else {
            session_label.trim().to_string()
        };
        let payload = json!({
            "model": self.model,
            "input": [
                {
                    "role": "system",
                    "content": [{
                        "type": "text",
                        "text": "You summarize Discord call transcripts into concise meeting notes. Respond with markdown bullet lists, call out action items, and keep the answer under 200 words.",
                    }]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": format!("Summarize the session titled '{label}'."),
                        },
                        {
                            "type": "input_file",
                            "file_id": file_id,
                        }
                    ]
                }
            ]
        });

        let response = self
            .client
            .post(RESPONSES_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .context("requesting transcript summary from OpenAI")?
            .error_for_status()
            .context("OpenAI summary request failed")?;

        let body: Value = response
            .json()
            .await
            .context("parsing OpenAI summary response")?;

        extract_summary_text(&body)
            .ok_or_else(|| anyhow!("OpenAI summary response did not include text: {}", body))
    }

    async fn cleanup_file(&self, file_id: &str) {
        let delete_url = format!("{FILES_ENDPOINT}/{}", file_id);
        let result = self
            .client
            .delete(delete_url)
            .bearer_auth(&self.api_key)
            .send()
            .await;
        if let Err(err) = result {
            warn!(?err, "Failed cleaning up uploaded transcript on OpenAI");
        }
    }
}

#[derive(Deserialize)]
struct FileUploadResponse {
    id: String,
}

fn extract_summary_text(value: &Value) -> Option<String> {
    value
        .get("output")
        .and_then(first_text)
        .or_else(|| value.get("content").and_then(first_text))
}

fn first_text(value: &Value) -> Option<String> {
    match value {
        Value::Array(items) => items.iter().find_map(first_text),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            if let Some(content) = map.get("content") {
                if let Some(text) = first_text(content) {
                    return Some(text);
                }
            }
            map.values().find_map(first_text)
        }
        _ => None,
    }
}
