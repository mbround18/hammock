use std::{collections::VecDeque, time::Instant};

use dashmap::DashMap;
use serenity::model::id::{ChannelId, GuildId, UserId};
use tokio::sync::Mutex;
use tracing::debug;

pub struct VoiceRoster {
    guild_id: GuildId,
    participants: DashMap<UserId, ParticipantRecord>,
    pending: Mutex<VecDeque<PendingJoin>>, // join order for grace window
}

impl VoiceRoster {
    pub fn new(guild_id: GuildId) -> Self {
        Self {
            guild_id,
            participants: DashMap::new(),
            pending: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn reset<I>(&self, channel_id: ChannelId, initial_users: I)
    where
        I: IntoIterator<Item = UserId>,
    {
        self.participants.clear();
        {
            let mut pending = self.pending.lock().await;
            pending.clear();
            let now = Instant::now();
            for user_id in initial_users {
                self.participants
                    .insert(user_id, ParticipantRecord::new(channel_id, now));
                pending.push_back(PendingJoin::new(user_id, channel_id, now));
            }
        }
        debug!(
            guild = %self.guild_id,
            channel = %channel_id,
            "Voice roster reset with {} initial participants",
            self.participants.len()
        );
    }

    pub async fn note_join(&self, channel_id: ChannelId, user_id: UserId) {
        let now = Instant::now();
        self.participants
            .insert(user_id, ParticipantRecord::new(channel_id, now));
        let mut pending = self.pending.lock().await;
        pending.retain(|entry| entry.user_id != user_id);
        pending.push_back(PendingJoin::new(user_id, channel_id, now));
        debug!(
            guild = %self.guild_id,
            channel = %channel_id,
            %user_id,
            "Participant joined tracked voice channel"
        );
    }

    pub async fn note_leave(&self, user_id: UserId) {
        self.participants.remove(&user_id);
        let mut pending = self.pending.lock().await;
        pending.retain(|entry| entry.user_id != user_id);
        debug!(guild = %self.guild_id, %user_id, "Participant left tracked channel");
    }

    pub async fn note_spoke(&self, user_id: UserId) {
        if let Some(mut entry) = self.participants.get_mut(&user_id) {
            entry.last_spoke_at = Some(Instant::now());
        }
        let mut pending = self.pending.lock().await;
        if let Some(pos) = pending.iter().position(|entry| entry.user_id == user_id) {
            pending.remove(pos);
        }
    }

    pub async fn guess_speaker(&self, channel_id: ChannelId) -> Option<UserId> {
        if let Some(user_id) = self.take_pending_for_channel(channel_id).await {
            debug!(
                guild = %self.guild_id,
                channel = %channel_id,
                %user_id,
                "Assigning pending join as speaker candidate"
            );
            return Some(user_id);
        }

        let mut matching_users = self
            .participants
            .iter()
            .filter(|entry| entry.value().channel_id == channel_id)
            .map(|entry| *entry.key());

        match matching_users.next() {
            Some(candidate) if matching_users.next().is_none() => {
                debug!(
                    guild = %self.guild_id,
                    channel = %channel_id,
                    %candidate,
                    "Single participant heuristic picked speaker"
                );
                Some(candidate)
            }
            _ => None,
        }
    }

    async fn take_pending_for_channel(&self, channel_id: ChannelId) -> Option<UserId> {
        let mut pending = self.pending.lock().await;
        if pending.len() == 1 && pending[0].channel_id == channel_id {
            return pending.pop_front().map(|entry| entry.user_id);
        }
        None
    }

    pub async fn clear(&self) {
        self.participants.clear();
        let mut pending = self.pending.lock().await;
        pending.clear();
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }
}

struct ParticipantRecord {
    channel_id: ChannelId,
    _joined_at: Instant,
    last_spoke_at: Option<Instant>,
}

impl ParticipantRecord {
    fn new(channel_id: ChannelId, joined_at: Instant) -> Self {
        Self {
            channel_id,
            _joined_at: joined_at,
            last_spoke_at: None,
        }
    }
}

struct PendingJoin {
    user_id: UserId,
    channel_id: ChannelId,
    _joined_at: Instant,
}

impl PendingJoin {
    fn new(user_id: UserId, channel_id: ChannelId, joined_at: Instant) -> Self {
        Self {
            user_id,
            channel_id,
            _joined_at: joined_at,
        }
    }
}
