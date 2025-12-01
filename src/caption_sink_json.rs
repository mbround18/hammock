use anyhow::Result;
use chrono::{DateTime, Local, Timelike};
use dashmap::{DashMap, mapref::entry::Entry};
use serde::{Deserialize, Serialize};
use serenity::model::id::{ChannelId, GuildId, UserId};
use std::fs;
use std::{
    io::{BufReader, BufWriter},
    path::PathBuf,
};

#[derive(Debug)]
pub struct CaptionSink {
    pub root: PathBuf,
    sessions: DashMap<(GuildId, ChannelId), SessionInfo>,
}

#[derive(Debug, Clone)]
struct SessionInfo {
    file_name: String,
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
    pub id: UserId,
    pub name: String,
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
                id,
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
        let slug = title.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Self::slugify_title(trimmed)
            }
        });
        let file_name = Self::build_file_name(guild_id, channel_id, now, slug.as_deref());
        let info = SessionInfo {
            file_name: file_name.clone(),
        };
        self.sessions.insert((guild_id, channel_id), info);
        Ok(self.root.join(file_name))
    }

    pub fn end_session(&self, guild_id: GuildId, channel_id: ChannelId) -> Option<PathBuf> {
        self.sessions
            .remove(&(guild_id, channel_id))
            .map(|(_, info)| self.root.join(info.file_name))
    }

    fn session_file_name(&self, guild_id: GuildId, channel_id: ChannelId) -> String {
        let now = Local::now();
        match self.sessions.entry((guild_id, channel_id)) {
            Entry::Occupied(entry) => entry.get().file_name.clone(),
            Entry::Vacant(vacant) => {
                let file_name = Self::build_file_name(guild_id, channel_id, now, None);
                vacant.insert(SessionInfo {
                    file_name: file_name.clone(),
                });
                file_name
            }
        }
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
        let file_path = dir.join(file_name);
        let mut entries = if file_path.exists() {
            let file = fs::File::open(&file_path)?;
            serde_json::from_reader(BufReader::new(file)).unwrap_or_else(|_| Vec::new())
        } else {
            Vec::new()
        };
        entries.push(entry);
        let file = fs::File::create(&file_path)?;
        serde_json::to_writer_pretty(BufWriter::new(file), &entries)?;
        Ok(())
    }
}
