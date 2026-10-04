use bevy::prelude::*;

use crate::{ConsoleCommand, ConsoleLine, ConsoleSettings, ConsoleState};

/// `mc_save`, `mc_load`, `mc_saves` and `mc_time`, handed to the Minecraft world, whose
/// answers come back as console lines.
pub(crate) fn route_minecraft_commands(
    mut events: MessageReader<ConsoleCommand>,
    mut output: (
        ResMut<ConsoleState>,
        Res<ConsoleSettings>,
        ResMut<ConsoleLine>,
    ),
    mut requests: MessageWriter<frame::McWorldCommand>,
    mut reports: MessageReader<frame::McWorldReport>,
) {
    let (console, settings, line) = &mut output;
    let capacity = settings.log_capacity;
    let mut echo = |msg: String| {
        line.0 = msg.clone();
        console.echo(msg, capacity);
    };
    for cmd in events.read() {
        let request = match (cmd.name.as_str(), cmd.args.as_slice()) {
            ("mc_save", [name]) => frame::McWorldCommand::Save(name.clone()),
            ("mc_load", [name]) => frame::McWorldCommand::Load(name.clone()),
            ("mc_saves", []) => frame::McWorldCommand::List,
            ("mc_time", [value]) => frame::McWorldCommand::Time(value.clone()),
            ("mc_slot", [slot]) => match slot.parse::<u8>() {
                Ok(slot @ 1..=9) => frame::McWorldCommand::Slot(slot),
                _ => {
                    echo("usage: mc_slot <1-9>".into());
                    continue;
                }
            },
            ("mc_use", []) => frame::McWorldCommand::Use,
            ("mc_time", _) => {
                echo("usage: mc_time <day|noon|night|midnight|ticks>".into());
                continue;
            }
            ("mc_save", _) => {
                echo("usage: mc_save <name>".into());
                continue;
            }
            ("mc_load", _) => {
                echo("usage: mc_load <name>".into());
                continue;
            }
            ("mc_saves", _) => {
                echo("usage: mc_saves".into());
                continue;
            }
            _ => continue,
        };
        requests.write(request);
    }
    for report in reports.read() {
        echo(report.0.clone());
    }
}
