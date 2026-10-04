//! The Minecraft map's inventory and hotbar, between the world that owns the
//! vanilla inventory (`render_anim`) and the HUD that draws it and takes the
//! player's clicks (`hud`).
use std::collections::HashMap;

use bevy::prelude::*;

/// Slots of the vanilla player inventory: 0..9 hotbar, 9..36 main, 36..40
/// armor (feet to head), 40 offhand.
pub const MC_INVENTORY_SLOTS: usize = 41;
pub const MC_HOTBAR: usize = 9;

/// One stack as the HUD shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct McStack {
    /// The item's id (`minecraft:dirt`), or `iw4:weapon/<index>` for a gun.
    pub id: String,
    pub count: u8,
    /// The MW2 weapon this item is, for a gun.
    pub weapon: Option<u32>,
    /// Durability left of the item's maximum, for a damaged tool.
    pub durability: Option<f32>,
}

/// A slot of the inventory screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McSlot {
    Inventory(usize),
    Crafting(usize),
    Result,
}

/// What the player did on the inventory screen, applied with vanilla's
/// container click rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McClick {
    /// A click on a slot; shift moves the stack across.
    Slot { slot: McSlot, right: bool, shift: bool },
    /// A click outside the window: throws the carried stack (or one of it).
    Outside { right: bool },
    /// A double click: gathers matching stacks onto the carried one.
    Gather { right: bool },
    /// A number key over a slot: swaps it with that hotbar slot.
    Swap { slot: McSlot, hotbar: usize },
    /// A drag across slots with a carried stack: shares it out.
    Spread { slots: Vec<usize>, right: bool },
    /// The screen closed: the carried stack and the crafting grid go back.
    Close,
}

#[derive(Resource, Default)]
pub struct MinecraftUi {
    /// A Minecraft world is in play and the player is alive.
    pub active: bool,
    /// The match's Minecraft world is still being generated; the loading
    /// screen holds until it is in, so its setup never lands mid-match.
    pub loading_world: bool,
    /// The inventory screen is open (the HUD's to change).
    pub inventory_open: bool,
    /// The selected hotbar slot.
    pub selected: usize,
    pub slots: Vec<Option<McStack>>,
    pub crafting: [Option<McStack>; 4],
    pub result: Option<McStack>,
    /// The stack on the cursor.
    pub cursor: Option<McStack>,
    /// Item icons: one atlas, and each item's rectangle in it (s0 t0 s1 t1).
    pub icons: Option<Handle<Image>>,
    pub icon_rects: HashMap<String, [f32; 4]>,
    /// Display names by item id.
    pub names: HashMap<String, String>,
    /// Clicks the HUD took this frame, for the inventory's owner.
    pub clicks: Vec<McClick>,
    /// A hotbar slot the player picked (scroll or number key).
    pub select: Option<usize>,
    /// A drop of the selected stack (`Q`; with the whole stack on control).
    pub drop_selected: Option<bool>,
    /// The mouse in the inventory's character box, -1..1 across and down,
    /// which the character's gaze follows.
    pub gaze: [f32; 2],
    /// The character box in window pixels (centre x and y, width, height),
    /// for placing the character.
    pub character_box: Option<[f32; 4]>,
    /// The name of the item just selected and how long ago, for the hotbar.
    pub selected_name: Option<(String, f32)>,
    /// The MW2 gun the hotbar selection asks the player to raise.
    pub weapon_request: Option<u32>,
    /// The selection is not a gun: the hand or a held item shows, and the
    /// gun neither fires nor aims.
    pub holding_item: bool,
    /// The selected slot is empty: MW2's bare hands show, without the gun.
    pub empty_hand: bool,
    /// How far through its swing the hand is, 0 to 1.
    pub hand_swing: f32,
    /// The minimap's picture of the world and the map points of its
    /// north-west and south-east corners.
    pub minimap: Option<(Handle<Image>, [f32; 2], [f32; 2])>,
    /// Vanilla's heart sprites side by side, 9 pixels each: container, full,
    /// half, and the container's hurt flash.
    pub hearts: Option<Handle<Image>>,
    /// The player's MW2 health and its most, for the hearts.
    pub health: Option<(f32, f32)>,
}

/// The player's own MW2 body, drawn standing in the inventory's character
/// box and looking towards the mouse.
#[derive(Resource, Default, Clone)]
pub struct InventoryPuppet {
    pub active: bool,
    pub client: u32,
    /// Where the body stands, in world space (in front of the camera).
    pub root: Mat4,
    /// The aim pitch its upper body and head take, in degrees.
    pub pitch: f32,
}

/// A Minecraft-map action with keys of its own. These keys sit in a layer
/// over the MW2 binds: on a Minecraft map a key bound here does this and not
/// the MW2 command it may also carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum McAction {
    Inventory,
    Drop,
    /// A hotbar slot, `0..MC_HOTBAR`.
    Hotbar(u8),
}

impl McAction {
    pub const ALL: [Self; 11] = [
        Self::Inventory,
        Self::Drop,
        Self::Hotbar(0),
        Self::Hotbar(1),
        Self::Hotbar(2),
        Self::Hotbar(3),
        Self::Hotbar(4),
        Self::Hotbar(5),
        Self::Hotbar(6),
        Self::Hotbar(7),
        Self::Hotbar(8),
    ];

    /// The command name the menus and `mcbind` use (`mc_inventory`,
    /// `mc_hotbar1`).
    pub fn command(self) -> String {
        match self {
            Self::Inventory => "mc_inventory".to_owned(),
            Self::Drop => "mc_drop".to_owned(),
            Self::Hotbar(slot) => format!("mc_hotbar{}", slot + 1),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|action| action.command() == name)
    }

    pub fn label(self) -> String {
        match self {
            Self::Inventory => "Inventory".to_owned(),
            Self::Drop => "Drop Item".to_owned(),
            Self::Hotbar(slot) => format!("Hotbar Slot {}", slot + 1),
        }
    }
}

/// The Minecraft actions whose keys went down this frame, from the Minecraft
/// bind layer.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct McKeyInput {
    pub inventory: bool,
    pub drop: bool,
    pub hotbar: Option<usize>,
}

/// A console request for the Minecraft world's saves
/// (`iw4l-artifacts/minecraft-saves/<name>`).
#[derive(Message, Clone, Debug)]
pub enum McWorldCommand {
    /// Writes the world, its seed and time, and the player's place and
    /// inventory under this name.
    Save(String),
    /// Loads the named save: the Minecraft map is loaded again on it.
    Load(String),
    /// Names the saves there are.
    List,
    /// Sets the time of day: `day`, `noon`, `night`, `midnight`, or game
    /// ticks since sunrise (0..24000).
    Time(String),
    /// Selects a hotbar slot (1..9).
    Slot(u8),
    /// One right click with what's held (places a block).
    Use,
}

/// A line the Minecraft world answers a `McWorldCommand` with, for the
/// console.
#[derive(Message, Clone, Debug)]
pub struct McWorldReport(pub String);

/// The host's arena terrain for joining clients (multiplayer peers can't
/// generate identical terrain themselves): each chunk column's encoded base
/// state, in the order to send them, nearest the spawn first.
#[derive(Resource, Default, Clone)]
pub struct McTerrainSource {
    /// Changes when the world does; a client's transfer starts over.
    pub generation: u64,
    /// Every arena chunk, nearest the spawn first.
    pub order: Vec<[i32; 2]>,
    /// The encoded chunks so far (`minecraft_terrain::terrain::encode_chunk`).
    pub chunks: HashMap<[i32; 2], std::sync::Arc<[u8]>>,
}

/// A piece of an arena chunk from the host, as a client receives it.
#[derive(Message, Clone, Debug)]
pub struct McChunkPart {
    pub generation: u32,
    pub pos: [i32; 2],
    pub part: u16,
    pub parts: u16,
    pub data: std::sync::Arc<[u8]>,
}

/// Items the host gives this client's Minecraft inventory (a refused
/// placement's block back).
#[derive(Message, Clone, Debug)]
pub struct McGrant {
    pub item: String,
    pub count: u8,
}

/// A block the host changed: its position and new state (`BlockStateId`,
/// the same on every peer with the same Minecraft files).
pub type McEdit = ([i32; 3], u16);

/// The host's Minecraft edit log: every block change in order, the first
/// with sequence 1. Clients get a compacted copy on joining, then the rest.
#[derive(Resource, Default, Clone)]
pub struct McEditLog {
    /// The world the edits belong to (`McTerrainSource::generation`).
    pub generation: u64,
    pub edits: Vec<McEdit>,
}

/// Edits from the host as a client receives them. `first_seq` 0 marks the
/// compacted state a joining client starts from; live edits follow in order.
#[derive(Message, Clone, Debug)]
pub struct McEditsReceived {
    pub generation: u32,
    pub first_seq: u32,
    pub edits: Vec<McEdit>,
}
