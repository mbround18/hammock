use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::fs;

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
        let transcript_text = self
            .load_transcript_text(file_path)
            .await
            .context("preparing transcript for summary upload")?;
        self.request_summary(&transcript_text, session_label).await
    }

    async fn load_transcript_text(&self, file_path: &Path) -> Result<String> {
        let bytes = fs::read(file_path)
            .await
            .with_context(|| format!("reading transcript {}", file_path.display()))?;
        flatten_transcript(&bytes)
    }

    async fn request_summary(&self, transcript: &str, session_label: &str) -> Result<String> {
        let label = if session_label.trim().is_empty() {
            "Discord session".to_string()
        } else {
            session_label.trim().to_string()
        };
        let truncated_transcript = truncate_transcript(transcript);
        let payload = json!({
            "model": self.model,
            "input": [
                {
                    "role": "system",
                    "content": [{
                        "type": "input_text",
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
                            "type": "input_text",
                            "text": truncated_transcript,
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
            .context("requesting transcript summary from OpenAI")?;

        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("reading OpenAI summary response body")?;

        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            return Err(anyhow!(
                "OpenAI summary request failed: status {status}, body: {body}"
            ));
        }

        let body: Value =
            serde_json::from_slice(&bytes).context("parsing OpenAI summary response")?;

        extract_summary_text(&body)
            .ok_or_else(|| anyhow!("OpenAI summary response did not include text: {}", body))
    }
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

fn flatten_transcript(bytes: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(bytes).context("parsing caption JSON")?;

    let mut buffer = String::with_capacity(bytes.len());

    if let Some(metadata) = value.get("metadata").and_then(Value::as_object) {
        if let Some(title) = metadata.get("title").and_then(Value::as_str) {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                buffer.push_str("Session Title: ");
                buffer.push_str(trimmed);
                buffer.push('\n');
            }
        }
        if let Some(started) = metadata.get("started_at").and_then(Value::as_str) {
            buffer.push_str("Started At: ");
            buffer.push_str(started);
            buffer.push('\n');
        }
        if let Some(ended) = metadata.get("ended_at").and_then(Value::as_str) {
            buffer.push_str("Ended At: ");
            buffer.push_str(ended);
            buffer.push('\n');
        }
        if let Some(duration) = metadata.get("duration_formatted").and_then(Value::as_str) {
            buffer.push_str("Duration: ");
            buffer.push_str(duration);
            buffer.push('\n');
        }
        buffer.push('\n');
    }

    buffer.push_str("Transcript:\n");

    let mut wrote_any = false;
    if let Some(entries) = value.get("transcriptions").and_then(Value::as_array) {
        for entry in entries {
            let timestamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("unknown time");
            let speaker = entry
                .get("speaker")
                .and_then(|speaker| speaker.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("Unknown Speaker");
            let comment = entry
                .get("comment")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();

            if comment.is_empty() {
                continue;
            }

            buffer.push('[');
            buffer.push_str(timestamp);
            buffer.push_str("] ");
            buffer.push_str(speaker);
            buffer.push_str(": ");
            buffer.push_str(comment);
            buffer.push('\n');
            wrote_any = true;
        }
    }

    if !wrote_any {
        bail!("transcript JSON did not contain any caption entries");
    }

    Ok(buffer)
}

fn truncate_transcript(transcript: &str) -> String {
    const MAX_CHARS: usize = 60_000;
    if transcript.len() <= MAX_CHARS {
        return transcript.to_string();
    }

    let mut truncated = transcript[..MAX_CHARS].to_string();
    truncated.push_str("\n\n[Transcript truncated]");
    truncated
}
