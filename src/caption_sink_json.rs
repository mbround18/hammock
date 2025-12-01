use anyhow::Result;
use chrono::{DateTime, Local, SecondsFormat, Timelike};
use dashmap::{DashMap, mapref::entry::Entry};
use serde::{Deserialize, Serialize};
use serenity::model::id::{ChannelId, GuildId, UserId};
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct CaptionSink {
    pub root: PathBuf,
    sessions: DashMap<(GuildId, ChannelId), SessionInfo>,
}

#[derive(Debug, Clone)]
struct SessionInfo {
    file_name: String,
    title: Option<String>,
    started_at: DateTime<Local>,
    started_instant: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub title: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_formatted: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionDocument {
    pub metadata: SessionMetadata,
    pub transcriptions: Vec<CaptionEntry>,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub file_path: PathBuf,
    pub title: Option<String>,
    pub started_at: DateTime<Local>,
    pub duration: Duration,
}

impl SessionSummary {
    pub fn duration_hms(&self) -> String {
        format_duration(self.duration)
    }

    pub fn date_label(&self) -> String {
        self.started_at.format("%m/%d/%Y").to_string()
    }
}

#[derive(Serialize, Deserialize)]
pub struct CaptionEntry {
    #[serde(with = "speaker_field")]
    pub speaker: SpeakerInfo,
    pub comment: String,
    pub timestamp: String,
}

#[derive(Serialize, Deserialize)]
pub struct SpeakerInfo {
    #[serde(default, with = "optional_user_id")]
    pub id: Option<UserId>,
    pub name: String,
}

mod optional_user_id {
    use super::UserId;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<UserId>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(id) => serializer.serialize_some(id),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<UserId>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<UserId>::deserialize(deserializer)?)
    }
}

mod speaker_field {
    use super::{SpeakerInfo, UserId};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawSpeaker {
        Detailed(SpeakerInfo),
        Legacy(UserId),
    }

    pub fn serialize<S>(speaker: &SpeakerInfo, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        speaker.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SpeakerInfo, D::Error>
    where
        D: Deserializer<'de>,
    {
        match RawSpeaker::deserialize(deserializer)? {
            RawSpeaker::Detailed(info) => Ok(info),
            RawSpeaker::Legacy(id) => Ok(SpeakerInfo {
                id: Some(id),
                name: format!("User {}", id.get()),
            }),
        }
    }
}

impl CaptionSink {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            sessions: DashMap::new(),
        }
    }

    pub fn start_session(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
        title: Option<String>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(&self.root)?;
        let now = Local::now();
        let clean_title = title.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let slug = clean_title.as_deref().and_then(Self::slugify_title);
        let file_name = Self::build_file_name(guild_id, channel_id, now, slug.as_deref());
        let info = SessionInfo {
            file_name: file_name.clone(),
            title: clean_title,
            started_at: now,
            started_instant: Instant::now(),
        };
        let path = self.root.join(&file_name);
        self.write_session_document(&path, &SessionDocument::new(&info))?;
        self.sessions.insert((guild_id, channel_id), info);
        Ok(path)
    }

    pub fn end_session(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Result<Option<SessionSummary>> {
        if let Some((_, info)) = self.sessions.remove(&(guild_id, channel_id)) {
            let file_path = self.root.join(&info.file_name);
            let mut document = self.load_session_document(&file_path, Some(&info))?;
            let duration = info.started_instant.elapsed();
            let ended_at = Local::now();
            document.metadata.title = info.title.clone();
            document.metadata.started_at = format_timestamp(info.started_at);
            document.metadata.ended_at = Some(format_timestamp(ended_at));
            document.metadata.duration_seconds = Some(duration.as_secs());
            document.metadata.duration_formatted = Some(format_duration(duration));
            self.write_session_document(&file_path, &document)?;
            return Ok(Some(SessionSummary {
                file_path,
                title: info.title.clone(),
                started_at: info.started_at,
                duration,
            }));
        }
        Ok(None)
    }

    fn session_file_name(&self, guild_id: GuildId, channel_id: ChannelId) -> String {
        let now = Local::now();
        match self.sessions.entry((guild_id, channel_id)) {
            Entry::Occupied(entry) => entry.get().file_name.clone(),
            Entry::Vacant(vacant) => {
                let file_name = Self::build_file_name(guild_id, channel_id, now, None);
                vacant.insert(SessionInfo {
                    file_name: file_name.clone(),
                    title: None,
                    started_at: now,
                    started_instant: Instant::now(),
                });
                file_name
            }
        }
    }

    fn session_info_snapshot(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Option<SessionInfo> {
        self.sessions
            .get(&(guild_id, channel_id))
            .map(|entry| entry.value().clone())
    }

    fn build_file_name(
        guild_id: GuildId,
        channel_id: ChannelId,
        timestamp: DateTime<Local>,
        title_slug: Option<&str>,
    ) -> String {
        let base = format!(
            "{}_{}_{}_{:02}{:02}{:02}",
            guild_id,
            channel_id,
            timestamp.format("%Y%m%d"),
            timestamp.hour(),
            timestamp.minute(),
            timestamp.second()
        );
        match title_slug {
            Some(slug) => format!("{}_{}.json", base, slug),
            None => format!("{}.json", base),
        }
    }

    fn slugify_title(input: &str) -> Option<String> {
        let mut slug = String::with_capacity(input.len());
        for ch in input.chars() {
            if slug.len() >= 48 {
                break;
            }
            if ch.is_ascii_alphanumeric() {
                slug.push(ch.to_ascii_lowercase());
            } else if ch.is_ascii_whitespace()
                || matches!(ch, '-' | '_') && !slug.ends_with('-') && !slug.is_empty()
            {
                slug.push('-');
            }
        }
        let slug = slug.trim_matches('-').to_string();
        if slug.is_empty() { None } else { Some(slug) }
    }

    pub fn append_json(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
        entry: CaptionEntry,
    ) -> Result<()> {
        let dir = &self.root;
        fs::create_dir_all(dir)?;
        let file_name = self.session_file_name(guild_id, channel_id);
        let file_path = dir.join(&file_name);
        let info = self.session_info_snapshot(guild_id, channel_id);
        let mut document = self.load_session_document(&file_path, info.as_ref())?;
        document.transcriptions.push(entry);
        self.write_session_document(&file_path, &document)?;
        Ok(())
    }

    pub fn relabel_placeholder(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
        placeholder: &str,
        new_id: UserId,
        new_name: &str,
    ) -> Result<bool> {
        let dir = &self.root;
        fs::create_dir_all(dir)?;
        let file_name = self.session_file_name(guild_id, channel_id);
        let file_path = dir.join(&file_name);
        if !file_path.exists() {
            return Ok(false);
        }

        let info = self.session_info_snapshot(guild_id, channel_id);
        let mut document = self.load_session_document(&file_path, info.as_ref())?;

        let mut updated = false;
        for entry in &mut document.transcriptions {
            if entry.speaker.id.is_none() && entry.speaker.name == placeholder {
                entry.speaker.id = Some(new_id);
                entry.speaker.name = new_name.to_string();
                updated = true;
            }
        }

        if updated {
            self.write_session_document(&file_path, &document)?;
        }

        Ok(updated)
    }

    fn load_session_document(
        &self,
        path: &Path,
        info: Option<&SessionInfo>,
    ) -> Result<SessionDocument> {
        if path.exists() {
            let contents = fs::read_to_string(path)?;
            if contents.trim().is_empty() {
                return Ok(SessionDocument::new_with_info(info));
            }
            if let Ok(document) = serde_json::from_str::<SessionDocument>(&contents) {
                return Ok(document);
            }
            if let Ok(entries) = serde_json::from_str::<Vec<CaptionEntry>>(&contents) {
                let mut document = SessionDocument::new_with_info(info);
                document.transcriptions = entries;
                return Ok(document);
            }
        }
        Ok(SessionDocument::new_with_info(info))
    }

    fn write_session_document(&self, path: &Path, document: &SessionDocument) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), document)?;
        Ok(())
    }
}

impl SessionInfo {
    fn initial_metadata(&self) -> SessionMetadata {
        SessionMetadata::new(self.title.clone(), self.started_at)
    }
}

impl SessionMetadata {
    fn new(title: Option<String>, started_at: DateTime<Local>) -> Self {
        Self {
            title,
            started_at: format_timestamp(started_at),
            ended_at: None,
            duration_seconds: None,
            duration_formatted: None,
        }
    }
}

impl SessionDocument {
    fn new(info: &SessionInfo) -> Self {
        Self {
            metadata: info.initial_metadata(),
            transcriptions: Vec::new(),
        }
    }

    fn new_with_info(info: Option<&SessionInfo>) -> Self {
        match info {
            Some(info) => Self::new(info),
            None => Self {
                metadata: SessionMetadata::new(None, Local::now()),
                transcriptions: Vec::new(),
            },
        }
    }
}

fn format_timestamp(value: DateTime<Local>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}
