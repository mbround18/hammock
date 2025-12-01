use std::{fs::OpenOptions, io::Write, path::PathBuf};

use chrono::{Datelike, Local};
use serenity::model::id::ChannelId;

#[derive(Debug)]
pub struct CaptionSink {
    root: PathBuf,
}

impl CaptionSink {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn append(&self, channel_id: ChannelId, line: &str) -> anyhow::Result<()> {
        let now = Local::now();
        let dir = self.root.join(channel_id.to_string());
        std::fs::create_dir_all(&dir)?;
        let file_name = format!("{:04}-{:02}-{:02}.txt", now.year(), now.month(), now.day());
        let file_path = dir.join(file_name);
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(file_path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }
}
