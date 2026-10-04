//! Streams the host's Minecraft arena terrain (`frame::McTerrainSource`) to
//! remote clients over the reliable lane: nearest the spawn first, a few
//! pieces a tick, never filling a client's reliable queue (game events and
//! action outcomes share it).
use std::collections::HashMap;

use bevy::prelude::*;

use crate::transport::reliable::{MAX_MC_CHUNK_PART, MAX_PENDING_RELIABLE, ReliableRow};

/// Reliable rows a client may have outstanding before terrain waits.
const TERRAIN_WINDOW: usize = MAX_PENDING_RELIABLE / 2;
/// Pieces sent to one client per tick.
const PIECES_PER_TICK: usize = 8;

/// Where each client's transfer stands: world generation, next chunk in the
/// source's order, next piece of it.
#[derive(Default)]
pub(crate) struct TerrainCursors(HashMap<sim::ClientId, (u64, usize, u16)>);

pub(crate) fn fanout_minecraft_terrain(
    source: Option<Res<frame::McTerrainSource>>,
    hub: Option<Res<crate::UdpAuthorityHub>>,
    mut reliable: ResMut<crate::ReliableEventHub>,
    mut cursors: Local<TerrainCursors>,
) {
    let (Some(source), Some(hub)) = (source, hub) else {
        cursors.0.clear();
        return;
    };
    let peers = hub.peer_clients();
    cursors.0.retain(|client, _| peers.contains(client));
    for client in peers {
        let cursor = cursors.0.entry(client).or_insert((source.generation, 0, 0));
        if cursor.0 != source.generation {
            *cursor = (source.generation, 0, 0);
        }
        let queue = reliable.queue_mut(client);
        let mut sent = 0;
        while sent < PIECES_PER_TICK && queue.pending_len() < TERRAIN_WINDOW {
            let Some(pos) = source.order.get(cursor.1) else {
                break;
            };
            // Not encoded yet: wait for it, keeping the order.
            let Some(data) = source.chunks.get(pos) else {
                break;
            };
            let parts = data.len().div_ceil(MAX_MC_CHUNK_PART).max(1);
            let start = usize::from(cursor.2) * MAX_MC_CHUNK_PART;
            let end = (start + MAX_MC_CHUNK_PART).min(data.len());
            queue.push(ReliableRow::McChunk {
                generation: source.generation as u32,
                pos: *pos,
                part: cursor.2,
                parts: parts as u16,
                data: data[start..end].into(),
            });
            sent += 1;
            cursor.2 += 1;
            if usize::from(cursor.2) >= parts {
                cursor.1 += 1;
                cursor.2 = 0;
            }
        }
    }
}
