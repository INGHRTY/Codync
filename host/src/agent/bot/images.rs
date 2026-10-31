//! The images a gateway turn carries in its message: its agent runs on another computer and can't open
//! this one's files. A room turn carries the message it answers.

use super::Actor;
use crate::chat::uploads;
use serde_json::Value;

impl Actor {
    /// The images the message this turn answers carries, as content blocks (a gateway's request turns them
    /// into `image_url` parts), and the names of the files it can't carry, which the prompt tells it about.
    pub(super) async fn turn_images(&self) -> (Vec<Value>, Vec<String>) {
        // A room turn answers the user's message in the room, whose files the group keeps.
        let root = match self.active_group.as_ref() {
            Some(_) => match self.hub.store.bot(&self.lane.chat) {
                Ok(Some(row)) => uploads::root(&row.config),
                _ => return (Vec::new(), Vec::new()),
            },
            None => uploads::root(&self.cfg),
        };
        let metas: Vec<Value> = self
            .answered()
            .iter()
            .filter_map(|id| self.hub.store.entry(id))
            .flat_map(|e| e.data["attachments"].as_array().cloned().unwrap_or_default())
            .collect();
        if metas.is_empty() {
            return (Vec::new(), Vec::new());
        }
        tokio::task::spawn_blocking(move || {
            let (images, mut named) = uploads::remote_plan(&root, &metas);
            let (blocks, unread) = uploads::image_blocks(&images);
            named.extend(unread);
            (blocks, named)
        })
        .await
        .unwrap_or_default()
    }

    /// The user entries this turn answers: the room's message in a room turn, its own messages otherwise.
    pub(super) fn answered(&self) -> &[String] {
        self.active_group.as_ref().map_or(self.turn_entries.as_slice(), |g| g.answers.as_slice())
    }
}
