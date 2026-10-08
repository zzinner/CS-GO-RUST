use std::collections::HashMap;
use std::path::PathBuf;

use bevy::{
    input::mouse::MouseWheel,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
    },
    prelude::*,
    window::{CursorGrabMode, PrimaryWindow},
};

use crate::{
    assets_loader::{AppState, SourceAssetConfig, SourceAssetManager},
    bsp::{MapLoadRequest, MapLoadResult},
    movement::{Player, PlayerControlSet},
    weapon::WeaponSystems,
};

pub(super) struct ConsolePlugin;

impl Plugin for ConsolePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConsoleState>()
            .init_resource::<ConVars>()
            .configure_sets(Update, ConsoleSet.before(PlayerControlSet))
            .configure_sets(Update, PlayerControlSet.after(ConsoleSet))
            .configure_sets(Update, WeaponSystems.after(PlayerControlSet))
            .add_systems(OnEnter(AppState::InGame), setup_console_ui)
            .add_systems(
                Update,
                (handle_console_input, update_console_ui)
                    .chain()
                    .in_set(ConsoleSet)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(Update, report_map_load_results)
            .add_systems(Update, discard_gameplay_input_during_loader);
    }
}

fn discard_gameplay_input_during_loader(
    state: Res<State<AppState>>,
    mut characters: EventReader<KeyboardInput>,
    mut mouse_wheel: EventReader<MouseWheel>,
) {
    if *state.get() != AppState::InGame {
        characters.clear();
        mouse_wheel.clear();
    }
}

fn parse_bunnyhopping_setting(value: &str, name: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("{name} expects 0 or 1")),
    }
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ConsoleSet;

#[derive(Resource, Debug)]
pub(super) struct ConVars {
    pub(super) gravity: f32,
    pub(super) jump_impulse: f32,
    pub(super) friction: f32,
    pub(super) accelerate: f32,
    pub(super) air_accelerate: f32,
    pub(super) max_speed: f32,
    pub(super) autobunnyhopping: bool,
    pub(super) enable_bunnyhopping: bool,
}

impl Default for ConVars {
    fn default() -> Self {
        Self {
            gravity: 800.0,
            jump_impulse: 301.993_38,
            friction: 5.2,
            accelerate: 5.5,
            air_accelerate: 12.0,
            max_speed: 320.0,
            autobunnyhopping: false,
            enable_bunnyhopping: false,
        }
    }
}

#[derive(Resource, Default)]
pub(super) struct ConsoleState {
    pub(super) open: bool,
    pub(super) closed_this_frame: bool,
    pub(super) input_buffer: String,
    pub(super) history: Vec<String>,
    pub(super) key_binds: HashMap<BindKey, String>,
    pub(super) show_fps: u8,
    pub(super) show_pos: bool,
    pub(super) jump_command_pressed: bool,
    pub(super) jump_command_just_pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BindKey {
    Keyboard(KeyCode),
    MouseWheelUp,
    MouseWheelDown,
}

impl ConsoleState {
    pub(super) fn accumulate_jump_input(&mut self, pressed: bool, just_pressed: bool) {
        self.jump_command_pressed |= pressed;
        self.jump_command_just_pressed |= just_pressed;
    }

    pub(super) fn consume_jump_input(&mut self) -> (bool, bool) {
        let input = (self.jump_command_pressed, self.jump_command_just_pressed);
        self.jump_command_pressed = false;
        self.jump_command_just_pressed = false;
        input
    }
}

#[derive(Component)]
struct ConsolePanel;

#[derive(Component)]
struct ConsoleHistoryText;

#[derive(Component)]
struct ConsoleInputText;

fn setup_console_ui(mut commands: Commands) {
    commands
        .spawn((
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(0.0),
                    height: Val::Percent(45.0),
                    padding: UiRect::all(Val::Px(14.0)),
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
                background_color: Color::srgba(0.015, 0.018, 0.025, 0.9).into(),
                visibility: Visibility::Hidden,
                ..default()
            },
            ConsolePanel,
        ))
        .with_children(|panel| {
            panel.spawn((
                TextBundle::from_section(
                    "",
                    TextStyle {
                        font_size: 17.0,
                        color: Color::srgb(0.88, 0.9, 0.93),
                        ..default()
                    },
                )
                .with_style(Style {
                    flex_grow: 1.0,
                    ..default()
                }),
                ConsoleHistoryText,
            ));
            panel.spawn((
                TextBundle::from_section(
                    "] ",
                    TextStyle {
                        font_size: 18.0,
                        color: Color::srgb(0.75, 0.88, 1.0),
                        ..default()
                    },
                ),
                ConsoleInputText,
            ));
        });
}

fn handle_console_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut characters: EventReader<KeyboardInput>,
    mut mouse_wheel: EventReader<MouseWheel>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut console: ResMut<ConsoleState>,
    mut convars: ResMut<ConVars>,
    mut assets: ResMut<SourceAssetConfig>,
    asset_manager: Res<SourceAssetManager>,
    mut players: Query<&mut Player>,
    mut map_requests: EventWriter<MapLoadRequest>,
) {
    console.closed_this_frame = false;
    let was_open = console.open;

    if keys.just_pressed(KeyCode::Backquote) {
        console.open = !console.open;
    }

    if console.open {
        if let Ok(mut window) = windows.get_single_mut() {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
    } else if was_open {
        console.closed_this_frame = true;
        if let Ok(mut window) = windows.get_single_mut() {
            window.cursor.grab_mode = CursorGrabMode::Locked;
            window.cursor.visible = false;
        }
    }

    if console.open {
        mouse_wheel.clear();
        for event in characters.read() {
            if event.state == ButtonState::Pressed {
                append_text_event(
                    &mut console.input_buffer,
                    event.key_code,
                    &event.logical_key,
                );
            }
        }
        append_console_space(&mut console.input_buffer, keys.just_pressed(KeyCode::Space));
        if keys.just_pressed(KeyCode::Backspace) {
            console.input_buffer.pop();
        }
        if keys.just_pressed(KeyCode::Escape) {
            console.open = false;
        } else if keys.just_pressed(KeyCode::Enter) {
            let line = std::mem::take(&mut console.input_buffer);
            if !line.trim().is_empty() {
                execute_command(
                    &line,
                    &mut console,
                    &mut convars,
                    &mut assets,
                    &asset_manager,
                    &mut players,
                    &mut map_requests,
                );
            }
        }
        if !console.open {
            console.closed_this_frame = true;
            if let Ok(mut window) = windows.get_single_mut() {
                window.cursor.grab_mode = CursorGrabMode::Locked;
                window.cursor.visible = false;
            }
        }
        return;
    }

    for _ in characters.read() {}
    let bound_commands: Vec<_> = console
        .key_binds
        .iter()
        .map(|(key, command)| (*key, command.clone()))
        .collect();
    let wheel_events: Vec<_> = mouse_wheel.read().map(|event| event.y).collect();
    for (key, command) in bound_commands {
        let keyboard_pressed = match key {
            BindKey::Keyboard(key) => keys.pressed(key),
            BindKey::MouseWheelUp | BindKey::MouseWheelDown => false,
        };
        let keyboard_just_pressed = match key {
            BindKey::Keyboard(key) => keys.just_pressed(key),
            BindKey::MouseWheelUp | BindKey::MouseWheelDown => false,
        };
        if command == "+jump" && keyboard_pressed {
            console.accumulate_jump_input(true, false);
        }
        if command == "+jump" && keyboard_just_pressed {
            console.accumulate_jump_input(true, true);
        }
        let wheel_triggered = match key {
            BindKey::MouseWheelUp => wheel_events.iter().any(|delta| *delta > 0.0),
            BindKey::MouseWheelDown => wheel_events.iter().any(|delta| *delta < 0.0),
            BindKey::Keyboard(_) => false,
        };
        if command == "+jump" && wheel_triggered {
            console.accumulate_jump_input(true, true);
        }
        if (keyboard_just_pressed || wheel_triggered) && command != "+jump" {
            execute_command(
                &command,
                &mut console,
                &mut convars,
                &mut assets,
                &asset_manager,
                &mut players,
                &mut map_requests,
            );
            if console.open {
                break;
            }
        }
    }
}

fn execute_command(
    line: &str,
    console: &mut ConsoleState,
    convars: &mut ConVars,
    assets: &mut SourceAssetConfig,
    asset_manager: &SourceAssetManager,
    players: &mut Query<&mut Player>,
    map_requests: &mut EventWriter<MapLoadRequest>,
) {
    let tokens = match parse_tokens(line) {
        Ok(tokens) => tokens,
        Err(error) => {
            console.history.push(format!("Error: {error}"));
            return;
        }
    };
    if tokens.is_empty() {
        return;
    }

    let result = match tokens[0].as_str() {
        "clear" if tokens.len() == 1 => {
            console.history.clear();
            return;
        }
        "echo" => {
            console.history.push(tokens[1..].join(" "));
            return;
        }
        "bind" if tokens.len() == 3 => match parse_bind_key(&tokens[1]) {
            Some(key) => {
                console.key_binds.insert(key, tokens[2].clone());
                Ok(format!("bound {:?} to {}", key, tokens[2]))
            }
            None => Err(format!("unknown key: {}", tokens[1])),
        },
        "unbind" if tokens.len() == 2 => match parse_bind_key(&tokens[1]) {
            Some(key) if console.key_binds.remove(&key).is_some() => {
                Ok(format!("unbound {:?}", key))
            }
            Some(key) => Err(format!("no binding for {:?}", key)),
            None => Err(format!("unknown key: {}", tokens[1])),
        },
        "sv_gravity" => set_float_convar(&tokens, "sv_gravity", &mut convars.gravity, false),
        "sv_jump_impulse" => {
            set_float_convar(&tokens, "sv_jump_impulse", &mut convars.jump_impulse, true)
        }
        "sv_friction" => set_float_convar(&tokens, "sv_friction", &mut convars.friction, false),
        "sv_accelerate" => {
            set_float_convar(&tokens, "sv_accelerate", &mut convars.accelerate, false)
        }
        "sv_airaccelerate" => set_float_convar(
            &tokens,
            "sv_airaccelerate",
            &mut convars.air_accelerate,
            false,
        ),
        "sv_maxspeed" => set_float_convar(&tokens, "sv_maxspeed", &mut convars.max_speed, true),
        "sv_autobunnyhopping" if tokens.len() == 2 => {
            parse_bunnyhopping_setting(&tokens[1], "sv_autobunnyhopping").map(|enabled| {
                convars.autobunnyhopping = enabled;
                format!("sv_autobunnyhopping = {}", u8::from(enabled))
            })
        }
        "sv_enablebunnyhopping" if tokens.len() == 2 => {
            parse_bunnyhopping_setting(&tokens[1], "sv_enablebunnyhopping").map(|enabled| {
                convars.enable_bunnyhopping = enabled;
                format!("sv_enablebunnyhopping = {}", u8::from(enabled))
            })
        }
        "noclip" if tokens.len() == 1 => match players.get_single_mut() {
            Ok(mut player) => {
                player.noclip = !player.noclip;
                Ok(format!(
                    "noclip {}",
                    if player.noclip { "ON" } else { "OFF" }
                ))
            }
            Err(_) => Err("player entity is unavailable".to_string()),
        },
        "+jump" if tokens.len() == 1 => {
            console.accumulate_jump_input(true, true);
            Ok("+jump".to_string())
        }
        "cl_showfps" if tokens.len() == 2 => match tokens[1].parse::<u8>() {
            Ok(value @ 0..=2) => {
                console.show_fps = value;
                Ok(format!("cl_showfps = {value}"))
            }
            _ => Err("cl_showfps expects 0, 1, or 2".to_string()),
        },
        "cl_showpos" if tokens.len() == 2 => match tokens[1].as_str() {
            "0" => {
                console.show_pos = false;
                Ok("cl_showpos = 0".to_string())
            }
            "1" => {
                console.show_pos = true;
                Ok("cl_showpos = 1".to_string())
            }
            _ => Err("cl_showpos expects 0 or 1".to_string()),
        },
        "path_csgo" if tokens.len() == 2 => {
            let path = PathBuf::from(&tokens[1]);
            assets.set_csgo_path(path.clone()).and_then(|_| {
                assets.save()?;
                Ok(format!(
                    "Depot path saved as {}; restart the app to mount it.",
                    path.display()
                ))
            })
        }
        "map" if tokens.len() == 2 => asset_manager.request_map(&tokens[1]).map(|_| {
            map_requests.send(MapLoadRequest {
                map_name: tokens[1].clone(),
            });
            format!("Loading map {}...", tokens[1])
        }),
        _ => Err(format!("unknown command or invalid arguments: {line}")),
    };

    match result {
        Ok(message) => console.history.push(message),
        Err(error) => console.history.push(format!("Error: {error}")),
    }
}

fn report_map_load_results(
    mut results: EventReader<MapLoadResult>,
    mut console: ResMut<ConsoleState>,
) {
    for result in results.read() {
        console.history.push(result.message.clone());
    }
}

fn set_float_convar(
    tokens: &[String],
    name: &str,
    value: &mut f32,
    strictly_positive: bool,
) -> Result<String, String> {
    if tokens.len() != 2 {
        return Err(format!("{name} expects one numeric value"));
    }
    let parsed = tokens[1]
        .parse::<f32>()
        .map_err(|_| format!("{name} expects a number"))?;
    let invalid = !parsed.is_finite()
        || if strictly_positive {
            parsed <= 0.0
        } else {
            parsed < 0.0
        };
    if invalid {
        return Err(format!("{name} value is out of range"));
    }
    *value = parsed;
    Ok(format!("{name} = {parsed}"))
}

fn parse_tokens(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut active = false;

    for character in line.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            active = true;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
            active = true;
        } else if character.is_whitespace() && !quoted {
            if active {
                tokens.push(std::mem::take(&mut token));
                active = false;
            }
        } else {
            token.push(character);
            active = true;
        }
    }

    if quoted || escaped {
        return Err("unterminated quoted argument".to_string());
    }
    if active {
        tokens.push(token);
    }
    Ok(tokens)
}

fn append_text_event(input_buffer: &mut String, key_code: KeyCode, logical_key: &Key) {
    if let Key::Character(character) = logical_key {
        if key_code != KeyCode::Backquote
            && key_code != KeyCode::Space
            && character.chars().all(|character| !character.is_control())
        {
            input_buffer.push_str(character);
        }
    }
}

fn append_console_space(input_buffer: &mut String, just_pressed: bool) {
    if just_pressed {
        input_buffer.push(' ');
    }
}

fn parse_bind_key(name: &str) -> Option<BindKey> {
    let upper = name.to_ascii_uppercase();
    Some(match upper.as_str() {
        "MWHEELUP" => BindKey::MouseWheelUp,
        "MWHEELDOWN" => BindKey::MouseWheelDown,
        _ => BindKey::Keyboard(match upper.as_str() {
            "SPACE" => KeyCode::Space,
            "BACKQUOTE" | "~" | "`" => KeyCode::Backquote,
            "ENTER" => KeyCode::Enter,
            "ESCAPE" => KeyCode::Escape,
            "TAB" => KeyCode::Tab,
            "LEFTSHIFT" | "SHIFT" => KeyCode::ShiftLeft,
            "LEFTCTRL" | "CTRL" => KeyCode::ControlLeft,
            "A" | "KEYA" => KeyCode::KeyA,
            "B" | "KEYB" => KeyCode::KeyB,
            "C" | "KEYC" => KeyCode::KeyC,
            "D" | "KEYD" => KeyCode::KeyD,
            "E" | "KEYE" => KeyCode::KeyE,
            "F" | "KEYF" => KeyCode::KeyF,
            "G" | "KEYG" => KeyCode::KeyG,
            "H" | "KEYH" => KeyCode::KeyH,
            "I" | "KEYI" => KeyCode::KeyI,
            "J" | "KEYJ" => KeyCode::KeyJ,
            "K" | "KEYK" => KeyCode::KeyK,
            "L" | "KEYL" => KeyCode::KeyL,
            "M" | "KEYM" => KeyCode::KeyM,
            "N" | "KEYN" => KeyCode::KeyN,
            "O" | "KEYO" => KeyCode::KeyO,
            "P" | "KEYP" => KeyCode::KeyP,
            "Q" | "KEYQ" => KeyCode::KeyQ,
            "R" | "KEYR" => KeyCode::KeyR,
            "S" | "KEYS" => KeyCode::KeyS,
            "T" | "KEYT" => KeyCode::KeyT,
            "U" | "KEYU" => KeyCode::KeyU,
            "V" | "KEYV" => KeyCode::KeyV,
            "W" | "KEYW" => KeyCode::KeyW,
            "X" | "KEYX" => KeyCode::KeyX,
            "Y" | "KEYY" => KeyCode::KeyY,
            "Z" | "KEYZ" => KeyCode::KeyZ,
            _ => return None,
        }),
    })
}

fn update_console_ui(
    console: Res<ConsoleState>,
    mut panels: Query<&mut Visibility, With<ConsolePanel>>,
    mut history_text: Query<&mut Text, (With<ConsoleHistoryText>, Without<ConsoleInputText>)>,
    mut input_text: Query<&mut Text, (With<ConsoleInputText>, Without<ConsoleHistoryText>)>,
) {
    if let Ok(mut visibility) = panels.get_single_mut() {
        *visibility = if console.open {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if let Ok(mut text) = history_text.get_single_mut() {
        text.sections[0].value = console
            .history
            .iter()
            .rev()
            .take(16)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Ok(mut text) = input_text.get_single_mut() {
        text.sections[0].value = format!("] {}", console.input_buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_parser_preserves_quoted_arguments() {
        assert_eq!(
            parse_tokens(r#"bind "Space" "+jump""#).unwrap(),
            vec!["bind", "Space", "+jump"]
        );
        assert_eq!(
            parse_tokens(r#"echo "hello world""#).unwrap(),
            vec!["echo", "hello world"]
        );
        assert!(parse_tokens(r#"echo "unfinished"#).is_err());
    }

    #[test]
    fn console_text_input_appends_space_once_and_ignores_backquote() {
        let mut input = "sv_gravity".to_string();
        append_text_event(&mut input, KeyCode::Space, &Key::Character(" ".into()));
        append_console_space(&mut input, true);
        append_text_event(&mut input, KeyCode::Digit8, &Key::Character("8".into()));
        append_text_event(&mut input, KeyCode::Digit0, &Key::Character("0".into()));
        append_text_event(&mut input, KeyCode::Digit0, &Key::Character("0".into()));
        append_text_event(&mut input, KeyCode::Backquote, &Key::Character("`".into()));
        assert_eq!(input, "sv_gravity 800");
    }

    #[test]
    fn key_parser_accepts_common_bevy_key_names() {
        assert_eq!(
            parse_bind_key("Space"),
            Some(BindKey::Keyboard(KeyCode::Space))
        );
        assert_eq!(parse_bind_key("V"), Some(BindKey::Keyboard(KeyCode::KeyV)));
        assert_eq!(
            parse_bind_key("Backquote"),
            Some(BindKey::Keyboard(KeyCode::Backquote))
        );
        assert_eq!(parse_bind_key("MWheelUp"), Some(BindKey::MouseWheelUp));
        assert_eq!(parse_bind_key("MWheelDown"), Some(BindKey::MouseWheelDown));
        assert_eq!(parse_bind_key("not-a-key"), None);
    }

    #[test]
    fn jump_input_pulses_persist_until_the_physics_system_consumes_them() {
        let mut state = ConsoleState::default();
        state.accumulate_jump_input(true, true);
        assert!(state.jump_command_pressed);
        assert!(state.jump_command_just_pressed);
        assert_eq!(state.consume_jump_input(), (true, true));
        assert_eq!(state.consume_jump_input(), (false, false));
    }

    #[test]
    fn convar_parser_rejects_invalid_values_and_accepts_zero_friction() {
        let mut friction = 5.2;
        assert!(
            set_float_convar(
                &["sv_friction".to_string(), "-1".to_string()],
                "sv_friction",
                &mut friction,
                false
            )
            .is_err()
        );
        assert!(
            set_float_convar(
                &["sv_friction".to_string(), "NaN".to_string()],
                "sv_friction",
                &mut friction,
                false
            )
            .is_err()
        );
        assert!(
            set_float_convar(
                &["sv_friction".to_string(), "0".to_string()],
                "sv_friction",
                &mut friction,
                false
            )
            .is_ok()
        );
        assert_eq!(friction, 0.0);
    }

    #[test]
    fn dynamic_convars_update_and_autobhop_is_explicitly_binary() {
        let mut vars = ConVars::default();
        assert!(
            set_float_convar(
                &["sv_gravity".to_string(), "800".to_string()],
                "sv_gravity",
                &mut vars.gravity,
                false
            )
            .is_ok()
        );
        assert_eq!(vars.gravity, 800.0);
        assert!(
            set_float_convar(
                &["sv_jump_impulse".to_string(), "300".to_string()],
                "sv_jump_impulse",
                &mut vars.jump_impulse,
                true
            )
            .is_ok()
        );
        assert_eq!(vars.jump_impulse, 300.0);
        assert_eq!(vars.max_speed, 320.0);
        assert!(
            set_float_convar(
                &["sv_maxspeed".to_string(), "250".to_string()],
                "sv_maxspeed",
                &mut vars.max_speed,
                true
            )
            .is_ok()
        );
        assert_eq!(vars.max_speed, 250.0);
        assert_eq!(
            parse_bunnyhopping_setting("1", "sv_autobunnyhopping"),
            Ok(true)
        );
        assert_eq!(
            parse_bunnyhopping_setting("0", "sv_enablebunnyhopping"),
            Ok(false)
        );
        assert!(parse_bunnyhopping_setting("true", "sv_autobunnyhopping").is_err());
    }
}
