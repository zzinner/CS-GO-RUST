use bevy::{
    input::mouse::MouseMotion,
    prelude::*,
    window::{CursorGrabMode, PrimaryWindow},
};

use crate::assets_loader::AppState;
use crate::audio_system::SoundCue;
use crate::console::{ConVars, ConsoleSet, ConsoleState};
use crate::environment::{Solid, SurfacePhysics, SurfaceType, WalkablePlane};
use crate::weapon::{DemoWeapon, WeaponState};

const PLAYER_EYE_HEIGHT_STANDING: f32 = 1.65;
const PLAYER_EYE_HEIGHT_CROUCHED: f32 = 1.05;
const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_TOP_OFFSET: f32 = 0.12;
pub(super) const DEFAULT_MAX_RUN_SPEED: f32 = 5.2;
const CROUCH_SPEED_FACTOR: f32 = 0.34;
const WALK_SPEED_FACTOR: f32 = 0.52;
const CROUCH_DURATION: f32 = 0.4;
const DUCK_FATIGUE_PER_TOGGLE: f32 = 0.75;
const DUCK_FATIGUE_RECOVERY: f32 = 0.35;
const MAX_DUCK_FATIGUE: f32 = 2.5;
const JUMP_BUFFER_FRAMES: f32 = 2.0;
const AIR_WISH_SPEED_CAP: f32 = 0.6;
const STOP_SPEED: f32 = 1.6;
const JUMP_HEIGHT: f32 = 1.125;
const MOUSE_SENSITIVITY: f32 = 0.002;
const MIN_MOVEMENT_SPEED: f32 = 0.01;
const MAX_FRAME_TIME: f32 = 0.05;
const MAX_CLIP_PLANES: usize = 4;
const CLIP_OVERBOUNCE: f32 = 1.001;
const CLIP_SKIN: f32 = 0.002;
const STEP_HEIGHT: f32 = 0.45;
const MIN_STEP_RISE: f32 = 0.01;
const MAX_GROUND_DROP: f32 = 0.25;
const MAX_GROUND_FOLLOW_RISE: f32 = 0.06;

pub(super) struct MovementPlugin;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PlayerControlSet;

impl Plugin for MovementPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), setup_player)
            .configure_sets(Update, PlayerControlSet.after(ConsoleSet))
            .add_systems(
                Update,
                (toggle_cursor, update_player)
                    .chain()
                    .in_set(PlayerControlSet)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
pub(super) struct Player {
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    pub(super) view_punch: Vec2,
    pub(super) velocity: Vec3,
    pub(super) grounded: bool,
    pub(super) health: u16,
    pub(super) crouch_fraction: f32,
    pub(super) eye_height: f32,
    pub(super) walking: bool,
    footstep_timer: f32,
    pub(super) noclip: bool,
    duck_timer: f32,
    duck_pressed: bool,
    jump_buffer_timer: f32,
    pub(super) ground_surface: SurfacePhysics,
    pub(super) ground_normal: Vec3,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            view_punch: Vec2::ZERO,
            velocity: Vec3::ZERO,
            grounded: true,
            health: 100,
            crouch_fraction: 0.0,
            eye_height: PLAYER_EYE_HEIGHT_STANDING,
            walking: false,
            footstep_timer: 0.0,
            noclip: false,
            duck_timer: 0.0,
            duck_pressed: false,
            jump_buffer_timer: 0.0,
            ground_surface: SurfacePhysics {
                friction: 1.0,
                jump_factor: 1.0,
                penetration_resistance: 0.0,
                surface_type: SurfaceType::Concrete,
            },
            ground_normal: Vec3::Y,
        }
    }
}

fn setup_player(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if let Ok(mut window) = windows.get_single_mut() {
        window.cursor.grab_mode = CursorGrabMode::Locked;
        window.cursor.visible = false;
    }

    commands
        .spawn((
            Camera3dBundle {
                transform: Transform::from_xyz(0.0, PLAYER_EYE_HEIGHT_STANDING, 10.0),
                ..default()
            },
            VisibilityBundle::default(),
            Player::default(),
            WeaponState::default(),
        ))
        .with_children(|camera| {
            camera
                .spawn(PbrBundle {
                    mesh: meshes.add(Cuboid::new(0.16, 0.16, 0.72)),
                    material: materials.add(Color::srgb(0.12, 0.13, 0.15)),
                    transform: Transform::from_xyz(0.28, -0.23, -0.48),
                    ..default()
                })
                .insert(DemoWeapon);
        });
}

fn toggle_cursor(
    keys: Res<ButtonInput<KeyCode>>,
    console: Res<ConsoleState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if console.open || console.closed_this_frame {
        return;
    }
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }

    if let Ok(mut window) = windows.get_single_mut() {
        let locked = window.cursor.grab_mode == CursorGrabMode::Locked;
        window.cursor.grab_mode = if locked {
            CursorGrabMode::None
        } else {
            CursorGrabMode::Locked
        };
        window.cursor.visible = locked;
    }
}

fn update_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut console: ResMut<ConsoleState>,
    convars: Res<ConVars>,
    mut sound_cues: EventWriter<SoundCue>,
    mut mouse_motion: EventReader<MouseMotion>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut player_query: Query<(&mut Transform, &mut Player, &WeaponState)>,
    solids: Query<(&GlobalTransform, &Solid), Without<Player>>,
) {
    if console.open || console.closed_this_frame {
        mouse_motion.clear();
        return;
    }
    let Ok(window) = windows.get_single() else {
        return;
    };
    let cursor_locked = window.cursor.grab_mode == CursorGrabMode::Locked;
    let Ok((mut transform, mut player, weapon)) = player_query.get_single_mut() else {
        return;
    };

    if cursor_locked {
        let mouse_delta = mouse_motion
            .read()
            .fold(Vec2::ZERO, |total, event| total + event.delta);
        player.yaw -= mouse_delta.x * MOUSE_SENSITIVITY;
        player.pitch = (player.pitch - mouse_delta.y * MOUSE_SENSITIVITY).clamp(-1.52, 1.52);
    } else {
        mouse_motion.clear();
    }

    let delta_seconds = time.delta_seconds().min(MAX_FRAME_TIME);
    let crouch_requested = keys.pressed(KeyCode::ControlLeft);
    if crouch_requested != player.duck_pressed {
        player.duck_timer = (player.duck_timer + DUCK_FATIGUE_PER_TOGGLE).min(MAX_DUCK_FATIGUE);
        player.duck_pressed = crouch_requested;
    }
    player.duck_timer = (player.duck_timer - DUCK_FATIGUE_RECOVERY * delta_seconds).max(0.0);
    let target_crouch_fraction = if crouch_requested { 1.0 } else { 0.0 };
    let next_crouch_fraction = update_crouch_fraction(
        player.crouch_fraction,
        target_crouch_fraction,
        delta_seconds,
        player.duck_timer,
    );
    if next_crouch_fraction < player.crouch_fraction {
        let standing_position =
            transform.translation + Vec3::Y * (PLAYER_EYE_HEIGHT_STANDING - player.eye_height);
        if !player.noclip
            && position_blocked(standing_position, PLAYER_EYE_HEIGHT_STANDING, &solids)
        {
            player.crouch_fraction = player.crouch_fraction.max(next_crouch_fraction);
        } else {
            player.crouch_fraction = next_crouch_fraction;
        }
    } else {
        player.crouch_fraction = next_crouch_fraction;
    }
    let next_eye_height = PLAYER_EYE_HEIGHT_STANDING
        + (PLAYER_EYE_HEIGHT_CROUCHED - PLAYER_EYE_HEIGHT_STANDING) * player.crouch_fraction;
    transform.translation.y += next_eye_height - player.eye_height;
    player.eye_height = next_eye_height;

    let max_speed = convars.max_speed * (weapon.max_player_speed / 250.0);
    let mut wish_speed = max_speed;
    player.walking = false;
    if crouch_requested {
        wish_speed = crouch_wish_speed(wish_speed);
    } else if keys.pressed(KeyCode::ShiftLeft) {
        wish_speed *= WALK_SPEED_FACTOR;
        player.walking = true;
    }

    let forward = Vec3::new(-player.yaw.sin(), 0.0, -player.yaw.cos());
    let right = Vec3::new(player.yaw.cos(), 0.0, -player.yaw.sin());
    let mut wish_direction = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        wish_direction += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        wish_direction -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        wish_direction += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        wish_direction -= right;
    }
    wish_direction = wish_direction.normalize_or_zero();

    let (bound_jump_pressed, bound_jump_just_pressed) = (
        console.jump_command_pressed,
        console.jump_command_just_pressed,
    );
    let jump_pressed = keys.just_pressed(KeyCode::Space) || bound_jump_just_pressed;
    let jump_held = keys.pressed(KeyCode::Space) || bound_jump_pressed;
    if player.noclip {
        update_noclip(&mut transform, &mut player, &keys, &convars, delta_seconds);
        console.consume_jump_input();
        return;
    }

    player.jump_buffer_timer = if convars.autobunnyhopping && jump_held {
        JUMP_BUFFER_FRAMES * delta_seconds
    } else {
        update_jump_buffer(player.jump_buffer_timer, jump_pressed, delta_seconds)
    };

    if player.grounded {
        let surface_friction = player.ground_surface.friction;
        let gravity = Vec3::NEG_Y * convars.gravity;
        let slope_acceleration =
            (gravity - player.ground_normal * gravity.dot(player.ground_normal)) * delta_seconds;
        player.velocity.x += slope_acceleration.x;
        player.velocity.z += slope_acceleration.z;
        apply_ground_friction(
            &mut player.velocity,
            surface_friction,
            convars.friction,
            delta_seconds,
        );
        accelerate_ground(
            &mut player.velocity,
            wish_direction,
            wish_speed,
            surface_friction,
            convars.accelerate,
            max_speed,
            delta_seconds,
        );
    } else {
        accelerate_air(
            &mut player.velocity,
            wish_direction,
            wish_speed,
            convars.air_accelerate,
            delta_seconds,
        );
    }

    let jump_requested = if convars.autobunnyhopping {
        jump_held
    } else {
        player.jump_buffer_timer > 0.0
    };
    if player.grounded && jump_requested {
        player.velocity.y = jump_impulse(convars.gravity, player.ground_surface.jump_factor);
        player.grounded = false;
        player.jump_buffer_timer = 0.0;
    }

    let was_grounded = player.grounded;
    let landing_speed = player.velocity.y;
    let stepped_up = move_horizontally(
        &mut transform,
        &mut player,
        delta_seconds,
        wish_direction,
        wish_speed,
        &solids,
    );
    move_vertically(
        &mut transform,
        &mut player,
        delta_seconds,
        stepped_up,
        jump_requested,
        &convars,
        &solids,
    );
    let hard_landing = !was_grounded && player.grounded && landing_speed < -2.0;
    if hard_landing {
        sound_cues.send(SoundCue::Footstep(player.ground_surface.surface_type));
        player.footstep_timer = 0.48;
    }
    player.footstep_timer = (player.footstep_timer - delta_seconds).max(0.0);
    if player.grounded
        && player.walking
        && Vec2::new(player.velocity.x, player.velocity.z).length() > MIN_MOVEMENT_SPEED
        && player.footstep_timer == 0.0
        && !hard_landing
    {
        sound_cues.send(SoundCue::Footstep(player.ground_surface.surface_type));
        player.footstep_timer = 0.48;
    }
    console.consume_jump_input();
}

fn update_noclip(
    transform: &mut Transform,
    player: &mut Player,
    keys: &ButtonInput<KeyCode>,
    convars: &ConVars,
    delta_seconds: f32,
) {
    let pitch = player.pitch + player.view_punch.x;
    let yaw = player.yaw + player.view_punch.y;
    let forward = Vec3::new(
        -yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    );
    let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
    let mut direction = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyW) {
        direction += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        direction -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        direction += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        direction -= right;
    }
    if keys.pressed(KeyCode::Space) {
        direction += Vec3::Y;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        direction -= Vec3::Y;
    }
    direction = direction.normalize_or_zero();
    player.velocity = direction * convars.max_speed;
    transform.translation += player.velocity * delta_seconds;
    player.grounded = false;
}

fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    current + (target - current).clamp(-max_delta, max_delta)
}

fn update_crouch_fraction(current: f32, target: f32, delta_seconds: f32, fatigue: f32) -> f32 {
    let transition_time = CROUCH_DURATION * (1.0 + fatigue.clamp(0.0, MAX_DUCK_FATIGUE));
    move_towards(current, target, delta_seconds / transition_time)
}

fn crouch_wish_speed(base_speed: f32) -> f32 {
    base_speed * CROUCH_SPEED_FACTOR
}

fn update_jump_buffer(timer: f32, just_pressed: bool, delta_seconds: f32) -> f32 {
    if just_pressed {
        JUMP_BUFFER_FRAMES * delta_seconds
    } else {
        (timer - delta_seconds).max(0.0)
    }
}

fn cap_landing_speed(velocity: &mut Vec3, max_speed: f32) {
    let horizontal_speed = Vec2::new(velocity.x, velocity.z).length();
    if horizontal_speed > max_speed {
        let scale = max_speed / horizontal_speed;
        velocity.x *= scale;
        velocity.z *= scale;
    }
}

fn cap_landing_speed_if_needed(
    velocity: &mut Vec3,
    max_speed: f32,
    grounded: bool,
    jump_requested: bool,
) {
    if grounded && !jump_requested {
        cap_landing_speed(velocity, max_speed);
    }
}

fn apply_ground_friction(
    velocity: &mut Vec3,
    friction_scale: f32,
    friction: f32,
    delta_seconds: f32,
) {
    let horizontal_speed = Vec2::new(velocity.x, velocity.z).length();
    if horizontal_speed <= MIN_MOVEMENT_SPEED {
        velocity.x = 0.0;
        velocity.z = 0.0;
        return;
    }

    let speed_drop = horizontal_speed.max(STOP_SPEED) * friction * friction_scale * delta_seconds;
    let new_speed = (horizontal_speed - speed_drop).max(0.0);
    let scale = new_speed / horizontal_speed;
    velocity.x *= scale;
    velocity.z *= scale;
}

fn accelerate_ground(
    velocity: &mut Vec3,
    wish_direction: Vec3,
    wish_speed: f32,
    surface_friction: f32,
    acceleration: f32,
    max_speed: f32,
    dt: f32,
) {
    if wish_speed <= 0.0 || wish_direction == Vec3::ZERO {
        return;
    }

    let speed_along_wish = velocity.dot(wish_direction);
    let speed_to_add = wish_speed - speed_along_wish;
    if speed_to_add <= 0.0 {
        return;
    }

    let acceleration_step = acceleration * max_speed * surface_friction * dt;
    *velocity += wish_direction * acceleration_step.min(speed_to_add);
}

fn accelerate_air(
    velocity: &mut Vec3,
    wish_direction: Vec3,
    wish_speed: f32,
    acceleration: f32,
    dt: f32,
) {
    if wish_speed <= 0.0 || wish_direction == Vec3::ZERO {
        return;
    }

    let speed_along_wish = velocity.dot(wish_direction);
    let speed_to_add = AIR_WISH_SPEED_CAP - speed_along_wish;
    if speed_to_add <= 0.0 {
        return;
    }

    let acceleration_step = acceleration * wish_speed * dt;
    *velocity += wish_direction * acceleration_step.min(speed_to_add);
}

fn jump_impulse(gravity: f32, jump_factor: f32) -> f32 {
    (2.0 * gravity * JUMP_HEIGHT * jump_factor).sqrt()
}

fn move_horizontally(
    transform: &mut Transform,
    player: &mut Player,
    dt: f32,
    wish_direction: Vec3,
    wish_speed: f32,
    solids: &Query<(&GlobalTransform, &Solid), Without<Player>>,
) -> bool {
    let mut remaining_time = dt;
    let mut clip_planes = [Vec3::ZERO; MAX_CLIP_PLANES];
    let mut clip_plane_count = 0;
    let mut stepped_up = false;

    for _ in 0..MAX_CLIP_PLANES {
        let delta = Vec3::new(player.velocity.x, 0.0, player.velocity.z) * remaining_time;
        if delta.length_squared() <= f32::EPSILON {
            break;
        }

        let Some((fraction, normal)) =
            earliest_horizontal_collision(transform.translation, delta, player.eye_height, solids)
        else {
            transform.translation += delta;
            break;
        };

        let distance = delta.length();
        let safe_fraction = (fraction - CLIP_SKIN / distance).max(0.0);
        let before_step = transform.translation;
        transform.translation += delta * safe_fraction;
        if should_attempt_step(player.grounded, wish_direction, wish_speed, normal)
            && try_step_up(
                &mut transform.translation,
                &mut player.velocity,
                delta * (1.0 - safe_fraction),
                before_step,
                normal,
                player.eye_height,
                player.grounded,
                wish_direction,
                wish_speed,
                solids,
            )
        {
            stepped_up = true;
            break;
        }

        if clip_plane_count < MAX_CLIP_PLANES {
            clip_planes[clip_plane_count] = normal;
            clip_plane_count += 1;
        }
        player.velocity = clip_velocity(player.velocity, normal, CLIP_OVERBOUNCE);
        remaining_time *= 1.0 - fraction;

        for plane in clip_planes.iter().take(clip_plane_count).copied() {
            if player.velocity.dot(plane) < 0.0 {
                player.velocity = clip_velocity(player.velocity, plane, CLIP_OVERBOUNCE);
            }
        }
    }
    stepped_up
}

fn move_vertically(
    transform: &mut Transform,
    player: &mut Player,
    dt: f32,
    stepped_up: bool,
    jump_requested: bool,
    convars: &ConVars,
    solids: &Query<(&GlobalTransform, &Solid), Without<Player>>,
) {
    let previous_y = transform.translation.y;
    let was_grounded = player.grounded;
    player.velocity.y -= convars.gravity * dt;
    let mut next_y = previous_y + player.velocity.y * dt;
    player.grounded = false;

    if player.velocity.y <= 0.0 {
        let previous_feet = previous_y - player.eye_height;
        let mut landing_surface: Option<(f32, SurfacePhysics, Vec3)> = None;
        for (solid_transform, solid) in solids.iter() {
            let support =
                support_surface_at(transform.translation, solid_transform.translation(), solid);
            let Some((height, normal)) = support else {
                continue;
            };
            if can_land_on_surface(
                was_grounded,
                stepped_up,
                previous_feet,
                next_y - player.eye_height,
                height,
            ) && landing_surface.map_or(true, |(highest, _, _)| height > highest)
            {
                landing_surface = Some((height, solid.surface, normal));
            }
        }

        if let Some((surface_height, surface, normal)) = landing_surface {
            let landing_height = surface_height + player.eye_height;
            if next_y <= landing_height {
                next_y = landing_height;
                player.ground_surface = surface;
                player.ground_normal = normal;
                player.velocity.y = 0.0;
                player.grounded = true;
                if !was_grounded {
                    cap_landing_speed_if_needed(
                        &mut player.velocity,
                        convars.max_speed,
                        true,
                        jump_requested,
                    );
                }
            }
        }
    } else {
        for (solid_transform, solid) in solids.iter() {
            if !horizontal_overlap(
                transform.translation,
                solid_transform.translation(),
                solid.half_extents,
            ) {
                continue;
            }

            let solid_bottom = solid_transform.translation().y - solid.half_extents.y;
            let previous_top = previous_y + PLAYER_TOP_OFFSET;
            let next_top = next_y + PLAYER_TOP_OFFSET;
            if previous_top <= solid_bottom && next_top >= solid_bottom {
                next_y = next_y.min(solid_bottom - PLAYER_TOP_OFFSET);
                player.velocity.y = 0.0;
                player.ground_normal = Vec3::Y;
            }
        }
    }

    transform.translation.y = next_y;
}

fn earliest_horizontal_collision(
    origin: Vec3,
    delta: Vec3,
    eye_height: f32,
    solids: &Query<(&GlobalTransform, &Solid), Without<Player>>,
) -> Option<(f32, Vec3)> {
    let mut nearest: Option<(f32, Vec3)> = None;

    for (transform, solid) in solids.iter() {
        let min = Vec3::new(
            transform.translation().x - solid.half_extents.x - PLAYER_RADIUS,
            transform.translation().y - solid.half_extents.y - PLAYER_TOP_OFFSET,
            transform.translation().z - solid.half_extents.z - PLAYER_RADIUS,
        );
        let max = Vec3::new(
            transform.translation().x + solid.half_extents.x + PLAYER_RADIUS,
            transform.translation().y + solid.half_extents.y + eye_height,
            transform.translation().z + solid.half_extents.z + PLAYER_RADIUS,
        );

        let Some(hit) = sweep_point_aabb(origin, delta, min, max) else {
            continue;
        };
        if solid.walkable_plane.is_some()
            && can_follow_walkable_plane(origin, delta, eye_height, solid.walkable_plane.unwrap())
        {
            continue;
        }
        if nearest.map_or(true, |(nearest_fraction, _)| hit.0 < nearest_fraction) {
            nearest = Some(hit);
        }
    }

    nearest
}

fn can_follow_walkable_plane(
    origin: Vec3,
    delta: Vec3,
    eye_height: f32,
    plane: WalkablePlane,
) -> bool {
    let feet = origin.y - eye_height;
    let destination = origin + delta;
    let Some(height) = plane.height_at(destination.x, destination.z) else {
        return false;
    };
    height - feet <= STEP_HEIGHT && feet - height <= MAX_GROUND_DROP
}

fn should_attempt_step(
    grounded: bool,
    wish_direction: Vec3,
    wish_speed: f32,
    normal: Vec3,
) -> bool {
    grounded
        && wish_speed > 0.0
        && wish_direction != Vec3::ZERO
        && wish_direction.dot(normal) < -0.01
        && normal.y.abs() < 0.001
}

fn can_land_on_surface(
    was_grounded: bool,
    stepped_up: bool,
    previous_feet: f32,
    next_feet: f32,
    height: f32,
) -> bool {
    let crossed_surface = !was_grounded && previous_feet >= height - 0.01 && next_feet <= height;
    let can_follow_slope = was_grounded
        && height - previous_feet <= MAX_GROUND_FOLLOW_RISE
        && previous_feet - height <= MAX_GROUND_DROP;
    let can_land_from_step = stepped_up
        && previous_feet - height <= STEP_HEIGHT
        && height <= previous_feet + STEP_HEIGHT + 0.05;
    crossed_surface || can_follow_slope || can_land_from_step
}

fn try_step_up(
    position: &mut Vec3,
    velocity: &mut Vec3,
    remaining_delta: Vec3,
    original_position: Vec3,
    wall_normal: Vec3,
    eye_height: f32,
    grounded: bool,
    wish_direction: Vec3,
    wish_speed: f32,
    solids: &Query<(&GlobalTransform, &Solid), Without<Player>>,
) -> bool {
    if !should_attempt_step(grounded, wish_direction, wish_speed, wall_normal) {
        return false;
    }

    let raised = *position + Vec3::Y * STEP_HEIGHT;
    if position_blocked(raised, eye_height, solids) {
        return false;
    }

    let mut candidate = raised;
    let mut remaining = remaining_delta;
    for _ in 0..MAX_CLIP_PLANES {
        if remaining.length_squared() <= f32::EPSILON {
            break;
        }
        let Some((fraction, normal)) =
            earliest_horizontal_collision(candidate, remaining, eye_height, solids)
        else {
            candidate += remaining;
            break;
        };
        if fraction <= 0.0 {
            return false;
        }

        let safe_fraction = (fraction - CLIP_SKIN / remaining.length()).max(0.0);
        candidate += remaining * safe_fraction;
        remaining = clip_velocity(remaining * (1.0 - fraction), normal, CLIP_OVERBOUNCE);
    }

    let original_feet = original_position.y - eye_height;
    let landing = solids
        .iter()
        .filter_map(|(solid_transform, solid)| {
            let (height, normal) =
                support_surface_at(candidate, solid_transform.translation(), solid)?;
            step_landing_position(candidate, eye_height, original_feet, height, normal)
        })
        .max_by(|(left_position, _), (right_position, _)| {
            left_position.y.total_cmp(&right_position.y)
        });
    let Some((candidate, normal)) = landing else {
        return false;
    };

    if position_blocked(candidate, eye_height, solids) {
        return false;
    }

    *position = candidate;
    *velocity = clip_velocity(*velocity, normal, CLIP_OVERBOUNCE);
    true
}

fn is_valid_step_landing(previous_feet: f32, height: f32, normal: Vec3) -> bool {
    normal.y >= std::f32::consts::FRAC_1_SQRT_2
        && height >= previous_feet + MIN_STEP_RISE
        && height <= previous_feet + STEP_HEIGHT + 0.05
}

fn step_landing_position(
    candidate: Vec3,
    eye_height: f32,
    previous_feet: f32,
    height: f32,
    normal: Vec3,
) -> Option<(Vec3, Vec3)> {
    is_valid_step_landing(previous_feet, height, normal).then(|| {
        (
            Vec3::new(candidate.x, height + eye_height, candidate.z),
            normal,
        )
    })
}

fn position_blocked(
    position: Vec3,
    eye_height: f32,
    solids: &Query<(&GlobalTransform, &Solid), Without<Player>>,
) -> bool {
    solids.iter().any(|(transform, solid)| {
        if solid.walkable_plane.is_some() {
            return false;
        }

        let min = transform.translation() - solid.half_extents;
        let max = transform.translation() + solid.half_extents;
        let player_min = Vec3::new(
            position.x - PLAYER_RADIUS,
            position.y - eye_height,
            position.z - PLAYER_RADIUS,
        );
        let player_max = Vec3::new(
            position.x + PLAYER_RADIUS,
            position.y + PLAYER_TOP_OFFSET,
            position.z + PLAYER_RADIUS,
        );
        player_min.x < max.x
            && player_max.x > min.x
            && player_min.y < max.y
            && player_max.y > min.y
            && player_min.z < max.z
            && player_max.z > min.z
    })
}

fn support_surface_at(
    player_position: Vec3,
    solid_position: Vec3,
    solid: &Solid,
) -> Option<(f32, Vec3)> {
    if let Some(plane) = solid.walkable_plane {
        return plane
            .height_at(player_position.x, player_position.z)
            .map(|height| (height, plane.normal));
    }

    if horizontal_overlap(player_position, solid_position, solid.half_extents) {
        Some((solid_position.y + solid.half_extents.y, Vec3::Y))
    } else {
        None
    }
}

fn sweep_point_aabb(origin: Vec3, delta: Vec3, min: Vec3, max: Vec3) -> Option<(f32, Vec3)> {
    let mut near = f32::NEG_INFINITY;
    let mut far = f32::INFINITY;
    let mut hit_normal = Vec3::ZERO;

    for axis in 0..3 {
        if delta[axis].abs() <= f32::EPSILON {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }

        let first = (min[axis] - origin[axis]) / delta[axis];
        let second = (max[axis] - origin[axis]) / delta[axis];
        let (entry, exit, normal_sign) = if first <= second {
            (first, second, -1.0)
        } else {
            (second, first, 1.0)
        };

        if entry > near {
            near = entry;
            hit_normal = Vec3::ZERO;
            hit_normal[axis] = normal_sign;
        }
        far = far.min(exit);
        if near > far {
            return None;
        }
    }

    if far < 0.0 || near < 0.0 || near > 1.0 || hit_normal == Vec3::ZERO {
        None
    } else {
        Some((near, hit_normal))
    }
}

fn clip_velocity(velocity: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    let into_surface = velocity.dot(normal);
    let backoff = if into_surface < 0.0 {
        into_surface * overbounce
    } else {
        into_surface / overbounce
    };
    let clipped = velocity - normal * backoff;
    Vec3::new(
        if clipped.x.abs() < 0.0001 {
            0.0
        } else {
            clipped.x
        },
        if clipped.y.abs() < 0.0001 {
            0.0
        } else {
            clipped.y
        },
        if clipped.z.abs() < 0.0001 {
            0.0
        } else {
            clipped.z
        },
    )
}

fn horizontal_overlap(position: Vec3, center: Vec3, half_extents: Vec3) -> bool {
    position.x + PLAYER_RADIUS > center.x - half_extents.x
        && position.x - PLAYER_RADIUS < center.x + half_extents.x
        && position.z + PLAYER_RADIUS > center.z - half_extents.z
        && position.z - PLAYER_RADIUS < center.z + half_extents.z
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_acceleration_builds_speed_toward_input_direction() {
        let mut velocity = Vec3::ZERO;
        accelerate_ground(
            &mut velocity,
            Vec3::X,
            DEFAULT_MAX_RUN_SPEED,
            1.0,
            5.5,
            DEFAULT_MAX_RUN_SPEED,
            0.015,
        );
        assert!(velocity.x > 0.0);
        assert_eq!(velocity.z, 0.0);
    }

    #[test]
    fn ground_acceleration_does_not_exceed_requested_speed() {
        let mut velocity = Vec3::X * (DEFAULT_MAX_RUN_SPEED - 0.1);
        for _ in 0..100 {
            accelerate_ground(
                &mut velocity,
                Vec3::X,
                DEFAULT_MAX_RUN_SPEED,
                1.0,
                5.5,
                DEFAULT_MAX_RUN_SPEED,
                0.015,
            );
        }
        assert!((velocity.length() - DEFAULT_MAX_RUN_SPEED).abs() < 0.001);
    }

    #[test]
    fn air_acceleration_limits_speed_added_in_the_wish_direction() {
        let mut velocity = Vec3::ZERO;
        accelerate_air(&mut velocity, Vec3::X, DEFAULT_MAX_RUN_SPEED, 12.0, 0.015);
        assert!((velocity.x - AIR_WISH_SPEED_CAP).abs() < 0.001);

        let previous_speed = velocity.length();
        accelerate_air(&mut velocity, Vec3::X, DEFAULT_MAX_RUN_SPEED, 12.0, 0.015);
        assert!((velocity.length() - previous_speed).abs() < 0.001);
    }

    #[test]
    fn ground_friction_reduces_speed_and_stops_small_residual_motion() {
        let mut velocity = Vec3::new(2.0, 0.0, 0.0);
        apply_ground_friction(&mut velocity, 1.0, 5.2, 0.1);
        assert!(velocity.x < 2.0);

        let mut slow_velocity = Vec3::new(0.02, 0.0, 0.0);
        apply_ground_friction(&mut slow_velocity, 1.0, 5.2, 0.1);
        assert_eq!(slow_velocity, Vec3::ZERO);
    }

    #[test]
    fn ground_friction_does_not_snap_high_speed_to_run_speed() {
        let mut velocity = Vec3::X * (DEFAULT_MAX_RUN_SPEED * 2.0);
        apply_ground_friction(&mut velocity, 1.0, 5.2, 0.01);

        assert!(velocity.x < DEFAULT_MAX_RUN_SPEED * 2.0);
        assert!(velocity.x > DEFAULT_MAX_RUN_SPEED);
    }

    #[test]
    fn lower_surface_friction_preserves_more_horizontal_speed() {
        let mut high_friction_velocity = Vec3::X * 4.0;
        let mut low_friction_velocity = high_friction_velocity;

        apply_ground_friction(&mut high_friction_velocity, 1.2, 5.2, 0.1);
        apply_ground_friction(&mut low_friction_velocity, 0.6, 5.2, 0.1);

        assert!(low_friction_velocity.x > high_friction_velocity.x);
    }

    #[test]
    fn jump_surface_factor_scales_takeoff_impulse() {
        assert!(jump_impulse(16.0, 1.2) > jump_impulse(16.0, 0.8));
        assert!((jump_impulse(16.0, 1.0) - (2.0 * 16.0 * JUMP_HEIGHT).sqrt()).abs() < 0.0001);
    }

    #[test]
    fn wall_clip_removes_only_velocity_into_wall() {
        let clipped = clip_velocity(Vec3::new(4.0, 0.0, -6.0), Vec3::Z, CLIP_OVERBOUNCE);
        assert!(clipped.x > 3.99);
        assert!(clipped.z.abs() < 0.01);
    }

    #[test]
    fn swept_box_hit_reports_entry_fraction_and_face_normal() {
        let hit = sweep_point_aabb(
            Vec3::ZERO,
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(5.0, -1.0, -1.0),
            Vec3::new(7.0, 1.0, 1.0),
        );
        assert_eq!(hit, Some((0.5, Vec3::NEG_X)));
    }

    #[test]
    fn grounded_player_can_follow_a_walkable_slope() {
        let plane = WalkablePlane {
            point: Vec3::new(0.0, 0.5, 0.0),
            normal: Vec3::new(0.0, 0.8944272, 0.4472136),
            half_bounds: Vec2::splat(4.0),
        };
        let origin = Vec3::new(0.0, PLAYER_EYE_HEIGHT_STANDING, 1.0);
        assert!(can_follow_walkable_plane(
            origin,
            Vec3::new(0.0, 0.0, -0.1),
            PLAYER_EYE_HEIGHT_STANDING,
            plane
        ));
        assert!(!can_follow_walkable_plane(
            origin,
            Vec3::new(0.0, 0.0, -6.0),
            PLAYER_EYE_HEIGHT_STANDING,
            plane
        ));
    }

    #[test]
    fn jump_buffer_accepts_press_on_contact_or_one_frame_before_landing() {
        let frame = 1.0 / 60.0;
        let buffered = update_jump_buffer(0.0, true, frame);
        assert!(buffered > 0.0);
        assert!(update_jump_buffer(buffered, false, frame) > 0.0);
        assert_eq!(update_jump_buffer(buffered, false, frame * 2.0), 0.0);
        assert_eq!(update_jump_buffer(0.0, false, frame), 0.0);
    }

    #[test]
    fn held_jump_does_not_refresh_the_buffer_after_the_initial_press() {
        let frame = 1.0 / 60.0;
        let buffered = update_jump_buffer(0.0, true, frame);
        let remaining = update_jump_buffer(buffered, false, frame);
        assert!(remaining > 0.0);
        assert_eq!(update_jump_buffer(remaining, false, frame), 0.0);
    }

    #[test]
    fn landing_speed_is_reduced_only_when_player_stays_grounded_without_jump_input() {
        let mut velocity = Vec3::new(10.0, -3.0, 0.0);
        cap_landing_speed_if_needed(&mut velocity, DEFAULT_MAX_RUN_SPEED, true, false);
        assert!((Vec2::new(velocity.x, velocity.z).length() - DEFAULT_MAX_RUN_SPEED).abs() < 0.001);
        assert_eq!(velocity.y, -3.0);

        let mut bunnyhop_velocity = Vec3::new(10.0, 0.0, 0.0);
        cap_landing_speed_if_needed(&mut bunnyhop_velocity, DEFAULT_MAX_RUN_SPEED, true, true);
        assert_eq!(bunnyhop_velocity.x, 10.0);

        let mut airborne_velocity = Vec3::new(10.0, 0.0, 0.0);
        cap_landing_speed_if_needed(&mut airborne_velocity, DEFAULT_MAX_RUN_SPEED, false, false);
        assert_eq!(airborne_velocity.x, 10.0);
    }

    #[test]
    fn crouch_speed_and_fatigue_slow_the_transition() {
        assert!(
            (crouch_wish_speed(DEFAULT_MAX_RUN_SPEED) - DEFAULT_MAX_RUN_SPEED * 0.34).abs()
                < 0.0001
        );
        let normal_progress = update_crouch_fraction(0.0, 1.0, 0.1, 0.0);
        let fatigued_progress = update_crouch_fraction(0.0, 1.0, 0.1, 2.0);
        assert!((normal_progress - 0.25).abs() < 0.0001);
        assert!(fatigued_progress < normal_progress);
    }

    #[test]
    fn step_uses_desired_input_into_a_vertical_plane_even_after_velocity_is_blocked() {
        let normal = Vec3::NEG_X;
        assert!(should_attempt_step(true, Vec3::X, 2.0, normal));
        assert!(!should_attempt_step(true, Vec3::ZERO, 2.0, normal));
        assert!(!should_attempt_step(true, Vec3::X, 0.0, normal));
        assert!(!should_attempt_step(true, Vec3::Z, 2.0, normal));
        assert!(!should_attempt_step(false, Vec3::X, 2.0, normal));
        assert!(!should_attempt_step(true, Vec3::X, 2.0, Vec3::Y));
    }

    #[test]
    fn grounded_player_does_not_snap_to_a_higher_surface_without_a_step() {
        assert!(!can_land_on_surface(true, false, 0.0, -0.01, 1.5));
        assert!(can_land_on_surface(true, false, 0.0, -0.01, 0.03));
        assert!(can_land_on_surface(true, true, 0.45, 0.44, 0.0));
        assert!(can_land_on_surface(true, true, 0.0, 0.02, 0.45));
        assert!(!can_land_on_surface(
            true,
            true,
            0.0,
            0.02,
            STEP_HEIGHT + 0.06
        ));
    }

    #[test]
    fn step_landing_snaps_to_walkable_surface_and_rejects_walls_or_overheight() {
        let raised_candidate = Vec3::new(2.0, 2.1, -3.0);
        let stepped_position = step_landing_position(
            raised_candidate,
            PLAYER_EYE_HEIGHT_STANDING,
            0.0,
            0.4,
            Vec3::Y,
        );
        assert_eq!(
            stepped_position,
            Some((
                Vec3::new(2.0, 0.4 + PLAYER_EYE_HEIGHT_STANDING, -3.0),
                Vec3::Y
            ))
        );
        assert_eq!(
            step_landing_position(
                raised_candidate,
                PLAYER_EYE_HEIGHT_STANDING,
                0.0,
                0.4,
                Vec3::X,
            ),
            None
        );
        assert_eq!(
            step_landing_position(
                raised_candidate,
                PLAYER_EYE_HEIGHT_STANDING,
                0.0,
                STEP_HEIGHT + 0.06,
                Vec3::Y,
            ),
            None
        );
    }
}
