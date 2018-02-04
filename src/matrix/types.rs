use std::collections::BTreeMap;
use serde_json;

#[derive(Clone, Debug, Deserialize)]
pub struct SyncResponse {
    pub next_batch: String,
    pub rooms: RoomsSyncResponse,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct RoomsSyncResponse {
    pub join: BTreeMap<String, JoinedRoomsSyncResponse>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct JoinedRoomsSyncResponse {
    pub timeline: RoomTimeline,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RoomTimeline {
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub etype: String,
    pub state_key: Option<String>,
    pub sender: String,
    pub origin_server_ts: u64,
    pub content: BTreeMap<String, serde_json::Value>,
}

impl SyncResponse {
    pub fn events<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a Event)> {
        self.rooms.join.iter().flat_map(|(room_id, entry)| {
            entry
                .timeline
                .events
                .iter()
                .map(move |ev| (room_id as &str, ev))
        })
    }
}

#[derive(Debug, Clone)]
pub struct SyncStreamItem {
    pub sync_response: SyncResponse,
    pub is_live: bool,
}
