mod assets_loader;
mod audio_system;
mod bsp;
mod console;
mod environment;
mod hud;
mod movement;
mod target;
mod weapon;

use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Kisak Strike Rust - Bevy prototype".to_string(),
                resolution: (1280.0_f32, 720.0_f32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(FrameTimeDiagnosticsPlugin)
        .add_plugins(assets_loader::SourceAssetLoaderPlugin::new())
        .add_plugins(bsp::BspMapPlugin)
        .add_plugins(console::ConsolePlugin)
        .add_plugins((
            environment::EnvironmentPlugin,
            movement::MovementPlugin,
            weapon::WeaponPlugin,
            audio_system::AudioSystemPlugin,
            hud::HudPlugin,
        ))
        .run();
}
