//! Streams the host's Minecraft world to remote clients over the reliable
//! lane: block edits (`frame::McEditLog`) first — a compacted copy of the
//! state so far for a client that joins, then every later edit in order —
//! and the arena's terrain (`frame::McTerrainSource`), nearest the spawn
//! first. A few rows a tick, never filling a client's reliable queue (game
//! events and action outcomes share it). One ordered lane carries both, so a
//! client sees edits and chunks in the order the host sent them.
use std::collections::HashMap;

use bevy::prelude::*;

use crate::transport::reliable::{MAX_MC_CHUNK_PART, MAX_MC_EDITS_PER_ROW, MAX_PENDING_RELIABLE, ReliableRow};

/// Reliable rows a client may have outstanding before the world waits.
const WORLD_WINDOW: usize = MAX_PENDING_RELIABLE / 2;
/// Terrain pieces sent to one client per tick.
const PIECES_PER_TICK: usize = 8;

/// One client's transfer.
#[derive(Default)]
struct Cursor {
    generation: u64,
    /// Next chunk in the terrain order, and next piece of it.
    chunk: usize,
    part: u16,
    /// The compacted state still to send, and the sequence it covers.
    compact: Option<(Vec<frame::McEdit>, u32)>,
    /// Next live edit's sequence, once the compacted state is out.
    next_edit: Option<u32>,
}

#[derive(Default)]
pub(crate) struct WorldCursors(HashMap<sim::ClientId, Cursor>);

pub(crate) fn fanout_minecraft_terrain(
    source: Option<Res<frame::McTerrainSource>>,
    log: Option<Res<frame::McEditLog>>,
    hub: Option<Res<crate::UdpAuthorityHub>>,
    mut reliable: ResMut<crate::ReliableEventHub>,
    mut cursors: Local<WorldCursors>,
) {
    let (Some(source), Some(log), Some(hub)) = (source, log, hub) else {
        cursors.0.clear();
        return;
    };
    if source.order.is_empty() {
        cursors.0.clear();
        return;
    }
    let peers = hub.peer_clients();
    cursors.0.retain(|client, _| peers.contains(client));
    let generation = source.generation as u32;
    let edits_ready = log.generation == source.generation;
    for client in peers {
        let cursor = cursors.0.entry(client).or_default();
        if cursor.generation != source.generation {
            *cursor = Cursor { generation: source.generation, ..Cursor::default() };
        }
        let queue = reliable.queue_mut(client);
        // Edits: the compacted state for a new client, then the rest.
        if edits_ready {
            if cursor.next_edit.is_none() && cursor.compact.is_none() {
                let through = log.edits.len() as u32;
                let mut latest: HashMap<[i32; 3], u16> = HashMap::new();
                for (pos, state) in &log.edits {
                    latest.insert(*pos, *state);
                }
                let mut state: Vec<frame::McEdit> = latest.into_iter().collect();
                state.sort_unstable();
                cursor.compact = Some((state, through));
            }
            if let Some((state, through)) = cursor.compact.as_mut() {
                while !state.is_empty() && queue.pending_len() < WORLD_WINDOW {
                    let take = state.len().min(MAX_MC_EDITS_PER_ROW);
                    let edits: Vec<frame::McEdit> = state.drain(..take).collect();
                    queue.push(ReliableRow::McEdits { generation, first_seq: 0, edits });
                }
                if state.is_empty() {
                    cursor.next_edit = Some(*through + 1);
                    cursor.compact = None;
                }
            }
            if let Some(next) = cursor.next_edit.as_mut() {
                while (*next as usize) <= log.edits.len() && queue.pending_len() < WORLD_WINDOW {
                    let start = *next as usize - 1;
                    let end = (start + MAX_MC_EDITS_PER_ROW).min(log.edits.len());
                    queue.push(ReliableRow::McEdits {
                        generation,
                        first_seq: *next,
                        edits: log.edits[start..end].to_vec(),
                    });
                    *next += (end - start) as u32;
                }
            }
        }
        // Terrain, nearest the spawn first.
        let mut sent = 0;
        while sent < PIECES_PER_TICK && queue.pending_len() < WORLD_WINDOW {
            let Some(pos) = source.order.get(cursor.chunk) else {
                break;
            };
            // Not encoded yet: wait for it, keeping the order.
            let Some(data) = source.chunks.get(pos) else {
                break;
            };
            let parts = data.len().div_ceil(MAX_MC_CHUNK_PART).max(1);
            let start = usize::from(cursor.part) * MAX_MC_CHUNK_PART;
            let end = (start + MAX_MC_CHUNK_PART).min(data.len());
            queue.push(ReliableRow::McChunk {
                generation,
                pos: *pos,
                part: cursor.part,
                parts: parts as u16,
                data: data[start..end].into(),
            });
            sent += 1;
            cursor.part += 1;
            if usize::from(cursor.part) >= parts {
                cursor.chunk += 1;
                cursor.part = 0;
            }
        }
    }
}
