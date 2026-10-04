use bevy::prelude::*;

use crate::{ConsoleCommand, ConsoleLine, ConsoleSettings, ConsoleState};

/// `mc_save`, `mc_load` and `mc_saves`, handed to the Minecraft world, whose
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
