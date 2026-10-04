use bevy::prelude::*;
use frame::ClientSet;

use crate::classes::store::{
    ClassStoreFile, load_class_store, save_class_store, sync_host_class_loadouts,
};

#[derive(Resource, Clone, Debug, Default)]
pub struct MenuMapList(pub Vec<asset_transport::MapPack>);

impl MenuMapList {
    pub fn maps(&self) -> impl Iterator<Item = &String> {
        self.0.iter().flat_map(|pack| &pack.maps)
    }

    pub fn contains(&self, map: &str) -> bool {
        self.maps().any(|installed| installed == map)
    }

    pub fn pack_of(&self, map: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|pack| pack.maps.iter().any(|installed| installed == map))
    }
}

pub(crate) struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuMapList>()
            .init_resource::<crate::ClassLoadoutCatalog>()
            .init_resource::<frame::GameSettings>()
            .init_resource::<crate::BindingView>()
            .init_resource::<crate::SessionClassStore>()
            .init_resource::<ClassStoreFile>()
            .init_resource::<frame::HostClassLoadouts>()
            .init_resource::<asset_game::MenuCatalog>()
            .init_resource::<asset_game::LocalizeCatalog>()
            .add_systems(
                Update,
                (
                    crate::options::apply_window_settings,
                    load_class_store,
                    sync_host_class_loadouts,
                    save_class_store,
                )
                    .chain()
                    .in_set(ClientSet::Ui),
            );
    }
}

pub fn install_frontend_menus(catalog: &mut asset_game::MenuCatalog) -> Result<(), String> {
    catalog.load_definitions(include_str!("../menus/frontend.json"))?;
    catalog.load_definitions(include_str!("../menus/connection_error.json"))?;
    catalog.load_definitions(include_str!("../menus/classes.json"))?;
    catalog.load_definitions(include_str!("../menus/settings.json"))?;
    catalog.load_definitions(include_str!("../menus/controller.json"))?;
    let has_controller_page = catalog.get("options_controller").is_some();
    for (name, menu) in &mut catalog.menus {
        if matches!(name.as_str(), "popup_endgame" | "popup_endgame_ranked") {
            for item in &mut menu.items {
                if item.name == "button_yes" {
                    item.handlers.action = vec![asset_game::MenuEvent::Script(
                        "play mouse_click; close self; exec \"disconnect\";".into(),
                    )];
                }
            }
        }
        if let Some(settings_link) = menu
            .items
            .iter()
            .find(|item| {
                item.item_type == 1
                    && matches!(item.text_key.as_str(), "@MENU_CHAT" | "@MENU_VOICE")
            })
            .cloned()
            && menu
                .items
                .iter()
                .any(|item| item.text_key == "@MENU_RESET_SYSTEM_DEFAULTS")
        {
            let mut multiplayer = settings_link;
            multiplayer.name = "multiplayer_settings".into();
            multiplayer.text_key = "@MENU_MULTIPLAYER_OPTIONS".into();
            multiplayer.rect.y = 88.0;
            multiplayer.vis_exp = "1".into();
            multiplayer.disabled_exp = "0".into();
            multiplayer.handlers.action = vec![asset_game::MenuEvent::Script(
                "play mouse_click; close self; open options_multi;".into(),
            )];
            let mut controller = multiplayer.clone();
            menu.items.push(multiplayer);
            if has_controller_page {
                controller.name = "controller_settings".into();
                controller.text_key = "Controller".into();
                controller.rect.y = 108.0;
                controller.handlers.action = vec![asset_game::MenuEvent::Script(
                    "play mouse_click; close self; open options_controller;".into(),
                )];
                menu.items.push(controller);
            }
        }

        let removed_rows: Vec<_> = menu
            .items
            .iter()
            .filter(|item| {
                item.item_type == 1
                    && matches!(
                        item.text_key.as_str(),
                        "@MENU_VOICE" | "@MENU_CHAT" | "@MENU_RESET_SYSTEM_DEFAULTS"
                    )
            })
            .map(|item| (item.rect.x, item.rect.y))
            .collect();
        menu.items.retain(|item| {
            !removed_rows.iter().any(|&(x, y)| {
                item.rect.x == x
                    && item.rect.y == y
                    && item.name != "multiplayer_settings"
                    && item.name != "controller_settings"
            })
        });
        if name == "pc_options_controls" {
            for item in &mut menu.items {
                if item.rect.x >= 232.0 && item.rect.y > 88.0 {
                    item.rect.y -= 20.0;
                }
            }
        }

        let Some(mode) = name.strip_prefix("settings_quick_") else {
            continue;
        };
        if sim::HostGameModeSelection::from_token(mode).is_none() {
            continue;
        }
        for item in &mut menu.items {
            if item.dvar == "camera_thirdperson" {
                if item.item_type == 12 {
                    item.choices.clear();
                    item.dvar.clear();
                    item.text_key = "Недоступно".into();
                }
                item.disabled_exp = "1".into();
                item.item_type = 0;
                item.static_flags |= 0x100000;
                item.handlers.action.clear();
                item.fore_color[3] = 0.4;
            } else if item.item_type == 1 && !item.dvar.is_empty() {
                item.text_scale = 0.30;
            }
        }
    }
    install_minecraft_controls(catalog);
    install_minecraft_difficulty(catalog);
    Ok(())
}

/// Options -> Controls -> Minecraft: the Actions page's rows rebound to the
/// Minecraft bind layer (`frame::McAction`), with a note naming any key the
/// layer shares with an MW2 action.
fn install_minecraft_controls(catalog: &mut asset_game::MenuCatalog) {
    const PAGE: &str = "pc_options_minecraft";
    let Some(mut page) = catalog.get("pc_options_actions").cloned() else {
        return;
    };
    page.name = PAGE.into();
    let mut rows: Vec<usize> = (0..page.items.len())
        .filter(|&i| page.items[i].item_type == 14)
        .collect();
    rows.sort_by(|&a, &b| page.items[a].rect.y.total_cmp(&page.items[b].rect.y));
    let mut dropped = Vec::new();
    let mut last_y = 0.0f32;
    for (row, &index) in rows.iter().enumerate() {
        let (x, y) = (page.items[index].rect.x, page.items[index].rect.y);
        let Some(action) = frame::McAction::ALL.get(row) else {
            dropped.push((x, y));
            continue;
        };
        last_y = y;
        page.items[index].dvar = action.command();
        if let Some(label) = page.items.iter_mut().find(|item| {
            item.item_type == 0 && item.rect.x == x && item.rect.y == y && !item.text_key.is_empty()
        }) {
            label.text_key = action.label();
        }
    }
    let first_dropped = dropped.iter().map(|&(_, y)| y).fold(f32::MAX, f32::min);
    page.items.retain(|item| {
        let row = dropped.iter().any(|&(x, y)| item.rect.x == x && item.rect.y == y);
        // The rule above the dropped scores row.
        let rule = item.rect.h == 1.0 && item.rect.y >= first_dropped;
        !row && !rule
    });
    let mut note = None;
    for item in &mut page.items {
        if item.text_key == "@MENU_ACTIONS" {
            item.text_key = "Minecraft".into();
            item.text_literal = true;
        } else if note.is_none() && item.item_type == 0 && item.rect.x == 232.0 && item.rect.y == last_y {
            note = Some(item.clone());
        }
    }
    if let Some(mut note) = note {
        note.name = "mc_bind_note".into();
        note.text_key.clear();
        note.text_exp = format!("op 38 s:{} op 1", hex("ui_mc_bind_note"));
        note.rect.y = last_y + 32.0;
        note.rect.h = 40.0;
        note.text_align_mode = 8;
        note.text_align_x = 4.0;
        note.text_scale = 0.3;
        note.static_flags = 1_048_576;
        note.fore_color = [0.75, 0.75, 0.75, 1.0];
        page.items.push(note);
    }
    catalog.menus.insert(PAGE.into(), page);

    let Some(controls) = catalog.menus.get_mut("pc_options_controls") else {
        return;
    };
    let Some(look) = controls.items.iter().find(|item| item.text_key == "@MENU_LOOK").cloned() else {
        return;
    };
    let below = look.rect.y + 20.0;
    for item in &mut controls.items {
        if item.rect.x >= 216.0 && item.rect.y >= below {
            item.rect.y += 20.0;
        }
    }
    let mut link = look;
    link.name = "minecraft_controls".into();
    link.text_key = "Minecraft".into();
    link.text_literal = true;
    link.rect.y = below;
    link.handlers.action = vec![asset_game::MenuEvent::Script(format!(
        "play mouse_click; open {PAGE};"
    ))];
    controls.items.push(link);
}

/// Game Setup's Minecraft rows, each a host rule the Minecraft world reads
/// when the match installs: difficulty (`scr_mc_difficulty`), the world
/// border (`scr_mc_border`, half width in blocks) and mobs (`scr_mc_mobs`;
/// auto is off when hosting for others) and building blocks (`scr_mc_blocks`:
/// a kit each life, or what you mine; auto is a kit when hosting). Each is a
/// button like its
/// neighbours whose text names the choice; the popup's panel grows to hold
/// them and the password row, which retail's four-row panel left out.
fn install_minecraft_difficulty(catalog: &mut asset_game::MenuCatalog) {
    const ROWS: [(&str, &str, &[(&str, &str)]); 5] = [
        ("mc_difficulty", "scr_mc_difficulty", &[
            ("MINECRAFT DIFFICULTY: PEACEFUL", "0"),
            ("MINECRAFT DIFFICULTY: EASY", "1"),
            ("MINECRAFT DIFFICULTY: NORMAL", "2"),
            ("MINECRAFT DIFFICULTY: HARD", "3"),
        ]),
        ("mc_border", "scr_mc_border", &[
            ("MINECRAFT BORDER: OFF", "0"),
            ("MINECRAFT BORDER: 32 BLOCKS", "32"),
            ("MINECRAFT BORDER: 64 BLOCKS", "64"),
            ("MINECRAFT BORDER: 96 BLOCKS", "96"),
            ("MINECRAFT BORDER: 128 BLOCKS", "128"),
        ]),
        ("mc_mobs", "scr_mc_mobs", &[
            ("MINECRAFT MOBS: AUTO", "auto"),
            ("MINECRAFT MOBS: ON", "1"),
            ("MINECRAFT MOBS: OFF", "0"),
        ]),
        ("mc_blocks", "scr_mc_blocks", &[
            ("MINECRAFT BLOCKS: AUTO", "auto"),
            ("MINECRAFT BLOCKS: KIT", "kit"),
            ("MINECRAFT BLOCKS: SURVIVAL", "survival"),
        ]),
        ("mc_limits", "scr_mc_limits", &[
            ("MINECRAFT MATCH: ENDLESS", "endless"),
            ("MINECRAFT MATCH: NORMAL LIMITS", "normal"),
        ]),
    ];
    let Some(setup) = catalog.menus.get_mut("lobby_game_setup") else {
        return;
    };
    let Some(template) = setup.items.iter().find(|item| item.name == "password_setup").cloned() else {
        return;
    };
    let row = template.rect.y;
    let added = ROWS.len() as f32 * 20.0;
    for item in &mut setup.items {
        if item.item_type == 1 && item.rect.x == template.rect.x && item.rect.y >= row {
            item.rect.y += added;
        }
        // The panel and its shadow are `24 + rows * 20` tall: retail's four,
        // the password row and these.
        for (_, expression) in &mut item.float_exp {
            *expression = expression.replace(
                "op 16 4 op 1 op 2 20",
                &format!("op 16 {} op 1 op 2 20", 5 + ROWS.len()),
            );
        }
    }
    for (_, expression) in &mut setup.float_exp {
        if let Ok(height) = expression.parse::<f32>()
            && height >= 140.0
        {
            *expression = (height + added).to_string();
        }
    }
    for (index, (name, dvar, choices)) in ROWS.iter().enumerate() {
        let mut choice = template.clone();
        choice.name = (*name).into();
        choice.item_type = 12;
        choice.dvar = (*dvar).into();
        choice.rect.y = row + index as f32 * 20.0;
        choice.choices = choices.iter().map(|(text, value)| ((*text).to_owned(), (*value).to_owned())).collect();
        choice.text_key.clear();
        choice.text_exp.clear();
        choice.handlers.action = vec![asset_game::MenuEvent::Script("play mouse_click;".into())];
        setup.items.push(choice);
    }
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}
