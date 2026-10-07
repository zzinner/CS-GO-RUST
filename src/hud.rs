use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::{Render, RenderApp, renderer::RenderAdapterInfo};
use std::sync::{Arc, Mutex};

use crate::{
    assets_loader::AppState,
    console::{ConVars, ConsoleState},
    movement::Player,
    weapon::{MAGAZINE_SIZE, WeaponState},
};

pub(super) struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        let backend_label = BackendLabel::default();
        app.insert_resource(backend_label.clone());
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(backend_label)
                .add_systems(Render, capture_backend_label);
        }
        app.add_systems(OnEnter(AppState::InGame), setup_hud)
            .add_systems(Update, update_hud.run_if(in_state(AppState::InGame)));
    }
}

#[derive(Resource, Clone)]
struct BackendLabel(Arc<Mutex<String>>);

impl Default for BackendLabel {
    fn default() -> Self {
        Self(Arc::new(Mutex::new("unavailable".to_string())))
    }
}

fn capture_backend_label(adapter: Res<RenderAdapterInfo>, label: Res<BackendLabel>) {
    if let Ok(mut backend) = label.0.lock() {
        *backend = format!("{:?}", adapter.backend);
    }
}

#[derive(Component)]
struct HealthReadout;

#[derive(Component)]
struct AmmoReadout;

#[derive(Component)]
struct SpeedReadout;

#[derive(Component)]
struct DebugReadout;

fn setup_hud(mut commands: Commands) {
    let text_style = TextStyle {
        font_size: 24.0,
        color: Color::WHITE,
        ..default()
    };
    commands.spawn((
        TextBundle::from_section("HEALTH: 100", text_style.clone()).with_style(Style {
            position_type: PositionType::Absolute,
            left: Val::Px(24.0),
            bottom: Val::Px(22.0),
            ..default()
        }),
        HealthReadout,
    ));
    commands.spawn((
        TextBundle::from_section("AMMO: 12 / 12", text_style.clone()).with_style(Style {
            position_type: PositionType::Absolute,
            right: Val::Px(24.0),
            bottom: Val::Px(22.0),
            ..default()
        }),
        AmmoReadout,
    ));
    commands.spawn((
        TextBundle::from_section(
            "SPEED: 000 u/s",
            TextStyle {
                font_size: 32.0,
                color: Color::srgb(0.95, 0.88, 0.54),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            width: Val::Px(250.0),
            left: Val::Percent(50.0),
            bottom: Val::Px(22.0),
            margin: UiRect::left(Val::Px(-125.0)),
            ..default()
        }),
        SpeedReadout,
    ));
    commands.spawn((
        TextBundle::from_section(
            "",
            TextStyle {
                font_size: 17.0,
                color: Color::srgb(0.8, 0.95, 0.8),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            left: Val::Px(24.0),
            top: Val::Px(20.0),
            ..default()
        }),
        DebugReadout,
    ));
}

fn source_units_per_second(horizontal_speed: f32, max_speed: f32) -> f32 {
    horizontal_speed * (250.0 / max_speed.max(f32::EPSILON))
}

fn update_hud(
    players: Query<(&Player, &Transform)>,
    weapons: Query<&WeaponState>,
    console: Res<ConsoleState>,
    convars: Res<ConVars>,
    diagnostics: Res<DiagnosticsStore>,
    backend_label: Res<BackendLabel>,
    mut health_text: Query<
        &mut Text,
        (
            With<HealthReadout>,
            Without<AmmoReadout>,
            Without<SpeedReadout>,
        ),
    >,
    mut ammo_text: Query<
        &mut Text,
        (
            With<AmmoReadout>,
            Without<HealthReadout>,
            Without<SpeedReadout>,
        ),
    >,
    mut speed_text: Query<
        &mut Text,
        (
            With<SpeedReadout>,
            Without<HealthReadout>,
            Without<AmmoReadout>,
        ),
    >,
    mut debug_text: Query<
        &mut Text,
        (
            With<DebugReadout>,
            Without<HealthReadout>,
            Without<AmmoReadout>,
            Without<SpeedReadout>,
        ),
    >,
) {
    if let Ok((player, transform)) = players.get_single() {
        if let Ok(mut text) = health_text.get_single_mut() {
            text.sections[0].value = format!("HEALTH: {}", player.health);
        }
        if let Ok(mut text) = speed_text.get_single_mut() {
            let horizontal_speed = Vec2::new(player.velocity.x, player.velocity.z).length();
            let source_speed = source_units_per_second(horizontal_speed, convars.max_speed);
            text.sections[0].value = format!("SPEED: {:03.0} u/s", source_speed);
        }
        if let Ok(mut text) = debug_text.get_single_mut() {
            let mut lines = Vec::new();
            if console.show_fps > 0 {
                let fps = diagnostics
                    .get(&FrameTimeDiagnosticsPlugin::FPS)
                    .and_then(|diagnostic| diagnostic.smoothed())
                    .unwrap_or(0.0);
                if console.show_fps == 1 {
                    lines.push(format!("FPS: {fps:.0}"));
                } else {
                    let backend = backend_label
                        .0
                        .lock()
                        .map(|backend| backend.clone())
                        .unwrap_or_else(|_| "unavailable".to_string());
                    lines.push(format!("FPS: {fps:.0} | Wgpu / {backend}"));
                }
            }
            if console.show_pos {
                lines.push(format!(
                    "pos: {:.3}, {:.3}, {:.3} | ang: {:.2}, {:.2}",
                    transform.translation.x,
                    transform.translation.y,
                    transform.translation.z,
                    player.yaw.to_degrees(),
                    player.pitch.to_degrees()
                ));
            }
            text.sections[0].value = lines.join("\n");
        }
    }

    if let Ok(weapon) = weapons.get_single() {
        if let Ok(mut text) = ammo_text.get_single_mut() {
            let reload_status = if weapon.is_reloading() {
                "  RELOADING..."
            } else {
                ""
            };
            text.sections[0].value = format!(
                "AMMO: {} / {}{}",
                weapon.ammunition(),
                MAGAZINE_SIZE,
                reload_status
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_run_speed_maps_to_250_source_units_per_second() {
        assert!((source_units_per_second(5.2, 5.2) - 250.0).abs() < 0.001);
    }
}
