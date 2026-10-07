use bevy::{
    prelude::*,
    render::{
        mesh::{Indices, PrimitiveTopology},
        render_asset::RenderAssetUsages,
    },
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
    window::{CursorGrabMode, PrimaryWindow},
};

use crate::{
    assets_loader::{
        AppState, ParsedVtf, SourceAssetManager, parse_vmt, parse_vtf, tokenize_key_values,
    },
    audio_system::SoundCue,
    console::{ConVars, ConsoleState},
    environment::Solid,
    movement::{Player, PlayerControlSet},
    target::{HitGroup, Hitbox, SHOT_DAMAGE, Target, scaled_damage},
};

pub(super) const MAGAZINE_SIZE: u8 = 12;
const SHOT_INTERVAL: f64 = 0.12;
const RELOAD_DURATION: f64 = 1.7;
const MAX_SHOT_RANGE: f32 = 70.0;
const TRACER_LIFETIME: f32 = 0.04;
const MAX_INACCURACY: f32 = 0.12;
const MOVEMENT_INACCURACY: f32 = 0.018;
const AIRBORNE_INACCURACY: f32 = 0.025;
const SHOT_INACCURACY: f32 = 0.008;
const INACCURACY_RECOVERY: f32 = 0.08;
const RECOIL_RECOVERY: f32 = 5.0;
const VIEW_PUNCH_RECOVERY: f32 = 8.0;
const PENETRATION_POWER: f32 = 3.5;

pub(super) struct WeaponPlugin;

impl Plugin for WeaponPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SourceWeaponImport>()
            .add_systems(
                OnEnter(AppState::InGame),
                (setup_tracer_assets, begin_source_weapon_import),
            )
            .configure_sets(Update, WeaponSystems.after(PlayerControlSet))
            .add_systems(
                Update,
                (
                    poll_source_weapon_import,
                    recover_weapon,
                    fire_hitscan,
                    apply_view_rotation,
                    tick_tracers,
                )
                    .chain()
                    .in_set(WeaponSystems)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct WeaponSystems;

#[derive(Component)]
pub(super) struct WeaponState {
    ammunition: u8,
    next_shot_at: f64,
    reload_finishes_at: Option<f64>,
    pub(super) recoil_index: f32,
    pub(super) current_inaccuracy: f32,
    pub(super) damage: u16,
    pub(super) cycle_time: f64,
    pub(super) max_player_speed: f32,
    pub(super) inaccuracy_move: f32,
    random_state: u64,
}

impl Default for WeaponState {
    fn default() -> Self {
        Self {
            ammunition: MAGAZINE_SIZE,
            next_shot_at: 0.0,
            reload_finishes_at: None,
            recoil_index: 0.0,
            current_inaccuracy: 0.0,
            damage: SHOT_DAMAGE,
            cycle_time: SHOT_INTERVAL,
            max_player_speed: 250.0,
            inaccuracy_move: MOVEMENT_INACCURACY,
            random_state: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

impl WeaponState {
    pub(super) fn ammunition(&self) -> u8 {
        self.ammunition
    }

    pub(super) fn is_reloading(&self) -> bool {
        self.reload_finishes_at.is_some()
    }

    fn start_reload(&mut self, now: f64) {
        if self.ammunition < MAGAZINE_SIZE && !self.is_reloading() {
            self.reload_finishes_at = Some(now + RELOAD_DURATION);
        }
    }

    fn update_reload(&mut self, now: f64) {
        if self
            .reload_finishes_at
            .is_some_and(|finishes_at| now >= finishes_at)
        {
            self.ammunition = MAGAZINE_SIZE;
            self.reload_finishes_at = None;
        }
    }

    fn try_fire(&mut self, now: f64) -> bool {
        self.update_reload(now);
        if self.is_reloading() || now < self.next_shot_at {
            return false;
        }
        if self.ammunition == 0 {
            self.start_reload(now);
            return false;
        }

        self.ammunition -= 1;
        self.next_shot_at = now + self.cycle_time;
        true
    }

    fn random_unit(&mut self) -> f32 {
        let mut value = self.random_state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.random_state = value;
        (value >> 40) as f32 / (1_u32 << 24) as f32
    }
}

#[derive(Resource)]
struct TracerAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

#[derive(Component)]
pub(super) struct DemoWeapon;

#[derive(Component)]
struct SourceWeapon;

#[derive(Resource, Default)]
struct SourceWeaponImport {
    task: Option<Task<Result<LoadedWeapon, String>>>,
}

struct LoadedWeapon {
    meshes: Vec<LoadedWeaponMesh>,
    materials: Vec<LoadedWeaponMaterial>,
    manifest: WeaponManifest,
}

struct LoadedWeaponMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    material_index: usize,
}

struct LoadedWeaponMaterial {
    name: String,
    texture: ParsedVtf,
    normal_map: Option<ParsedVtf>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WeaponManifest {
    damage: u16,
    cycle_time: f64,
    max_player_speed: f32,
    inaccuracy_move: f32,
}

fn begin_source_weapon_import(
    manager: Res<SourceAssetManager>,
    mut import: ResMut<SourceWeaponImport>,
) {
    if import.task.is_some() || manager.mounted_root.is_none() {
        return;
    }
    let manager = manager.clone();
    import.task =
        Some(AsyncComputeTaskPool::get().spawn(async move { load_source_weapon(&manager) }));
}

fn poll_source_weapon_import(
    mut commands: Commands,
    mut import: ResMut<SourceWeaponImport>,
    mut weapon_query: Query<&mut WeaponState>,
    players: Query<Entity, With<Player>>,
    demo_weapons: Query<Entity, With<DemoWeapon>>,
    source_weapons: Query<Entity, With<SourceWeapon>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(task) = import.task.as_mut() else {
        return;
    };
    let Some(result) = future::block_on(future::poll_once(task)) else {
        return;
    };
    import.task = None;

    match result {
        Ok(loaded) => {
            let Ok(player_entity) = players.get_single() else {
                error!("AK-47 import completed, but there is not exactly one player camera");
                return;
            };
            let material_handles: Vec<_> = loaded
                .materials
                .iter()
                .map(|material| {
                    info!(
                        "Loaded AK-47 material {} ({}x{}, {} mip levels, {:?})",
                        material.name,
                        material.texture.width,
                        material.texture.height,
                        material.texture.mip_count,
                        material.texture.pixel_format
                    );
                    let base_texture = images.add(material.texture.image.clone());
                    let normal_map_texture = material
                        .normal_map
                        .as_ref()
                        .map(|normal_map| images.add(normal_map.image.clone()));
                    materials.add(StandardMaterial {
                        base_color_texture: Some(base_texture),
                        normal_map_texture,
                        metallic: 0.0,
                        perceptual_roughness: 0.8,
                        ..default()
                    })
                })
                .collect();

            for mesh_data in loaded.meshes {
                let mut mesh = Mesh::new(
                    PrimitiveTopology::TriangleList,
                    RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, mesh_data.positions);
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, mesh_data.normals);
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, mesh_data.uvs);
                mesh.insert_indices(Indices::U32(mesh_data.indices));
                let Some(material) = material_handles.get(mesh_data.material_index) else {
                    error!(
                        "AK-47 mesh references unavailable material slot {}",
                        mesh_data.material_index
                    );
                    continue;
                };
                let mesh_handle = meshes.add(mesh);
                commands.entity(player_entity).with_children(|camera| {
                    camera.spawn((
                        PbrBundle {
                            mesh: mesh_handle,
                            material: material.clone(),
                            transform: Transform {
                                translation: Vec3::new(0.24, -0.2, -0.45),
                                rotation: Quat::IDENTITY,
                                scale: Vec3::splat(0.01),
                            },
                            ..default()
                        },
                        SourceWeapon,
                    ));
                });
            }

            for entity in &demo_weapons {
                commands.entity(entity).despawn_recursive();
            }
            for entity in &source_weapons {
                commands.entity(entity).despawn_recursive();
            }
            if let Ok(mut weapon) = weapon_query.get_single_mut() {
                weapon.damage = loaded.manifest.damage;
                weapon.cycle_time = loaded.manifest.cycle_time;
                weapon.max_player_speed = loaded.manifest.max_player_speed;
                weapon.inaccuracy_move = loaded.manifest.inaccuracy_move;
                info!(
                    "Applied AK-47 manifest: damage {}, cycle {:.3}s, max speed {:.1}, move inaccuracy {:.4}",
                    weapon.damage,
                    weapon.cycle_time,
                    weapon.max_player_speed,
                    weapon.inaccuracy_move
                );
            }
        }
        Err(error_message) => {
            error!("Could not import AK-47 from the mounted depot: {error_message}");
        }
    }
}

fn load_source_weapon(manager: &SourceAssetManager) -> Result<LoadedWeapon, String> {
    let model_path = "models/weapons/v_rif_ak47.mdl";
    let model_stem = model_path
        .strip_suffix(".mdl")
        .ok_or_else(|| format!("Invalid model path {model_path}"))?;
    let limits = vformats::Limits::default();
    let mdl_bytes = manager.read_asset(model_path)?;
    let vvd_bytes = manager.read_asset(&format!("{model_stem}.vvd"))?;
    let vtx_bytes = manager.read_asset(&format!("{model_stem}.dx90.vtx"))?;
    let mdl = vformats::mdl::parse_mdl(&mdl_bytes, &limits)
        .map_err(|error| format!("Could not parse {model_path}: {error}"))?;
    let vvd = vformats::mdl::parse_vvd(&vvd_bytes, &limits)
        .map_err(|error| format!("Could not parse {model_stem}.vvd: {error}"))?;
    let vtx = vformats::mdl::parse_vtx(&vtx_bytes, &limits)
        .map_err(|error| format!("Could not parse {model_stem}.dx90.vtx: {error}"))?;
    let read = vformats::mdl::assemble(&mdl, &vvd, &vtx)
        .map_err(|error| format!("Could not assemble {model_path}: {error}"))?;
    if read.model.meshes.is_empty() || read.model.material_names.is_empty() {
        return Err(format!(
            "{model_path} contains no renderable meshes/materials"
        ));
    }

    let mut imported_materials = Vec::with_capacity(read.model.material_names.len());
    for material_name in &read.model.material_names {
        let (source_path, vmt_bytes) =
            find_material_vmt(manager, &read.model.material_dirs, material_name)?;
        let vmt_text = std::str::from_utf8(&vmt_bytes)
            .map_err(|error| format!("{source_path} is not UTF-8 VMT text: {error}"))?;
        let material = parse_vmt(vmt_text)
            .map_err(|error| format!("Could not parse {source_path}: {error}"))?;
        let base_texture = material
            .base_texture
            .ok_or_else(|| format!("{source_path} has no $basetexture"))?;
        let texture_bytes = manager.read_asset(&base_texture).map_err(|error| {
            format!("Could not load $basetexture {base_texture} from {source_path}: {error}")
        })?;
        let texture = parse_vtf(&texture_bytes)
            .map_err(|error| format!("Could not parse {base_texture}: {error}"))?;
        let normal_map = material
            .bump_map
            .map(|path| {
                let bytes = manager.read_asset(&path).map_err(|error| {
                    format!("Could not load $bumpmap {path} from {source_path}: {error}")
                })?;
                parse_vtf(&bytes).map_err(|error| format!("Could not parse {path}: {error}"))
            })
            .transpose()?;
        imported_materials.push(LoadedWeaponMaterial {
            name: material_name.clone(),
            texture,
            normal_map,
        });
    }

    let meshes = read
        .model
        .meshes
        .into_iter()
        .map(|mesh| LoadedWeaponMesh {
            positions: mesh.vertices.iter().map(|vertex| vertex.position).collect(),
            normals: mesh.vertices.iter().map(|vertex| vertex.normal).collect(),
            uvs: mesh.vertices.iter().map(|vertex| vertex.uv).collect(),
            indices: mesh.indices,
            material_index: mesh.material_index,
        })
        .collect();
    let manifest_bytes = manager.read_asset("scripts/weapon_ak47.txt")?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)
        .map_err(|error| format!("scripts/weapon_ak47.txt is not UTF-8: {error}"))?;
    let manifest = parse_weapon_manifest(manifest_text)?;

    Ok(LoadedWeapon {
        meshes,
        materials: imported_materials,
        manifest,
    })
}

fn find_material_vmt(
    manager: &SourceAssetManager,
    material_dirs: &[String],
    material_name: &str,
) -> Result<(String, Vec<u8>), String> {
    let material_name = material_name.trim_start_matches("materials/");
    let material_name = material_name.strip_suffix(".vmt").unwrap_or(material_name);
    let mut candidates = Vec::new();
    for directory in material_dirs {
        let directory = directory.trim_matches('/').replace('\\', "/");
        if !directory.is_empty() {
            candidates.push(format!("materials/{directory}/{material_name}.vmt"));
        }
    }
    candidates.push(format!("materials/{material_name}.vmt"));

    let mut failures = Vec::new();
    for candidate in candidates {
        match manager.read_asset(&candidate) {
            Ok(bytes) => return Ok((candidate, bytes)),
            Err(error_message) => failures.push(error_message),
        }
    }
    Err(format!(
        "No VMT found for model material {material_name}: {}",
        failures.join("; ")
    ))
}

fn parse_weapon_manifest(text: &str) -> Result<WeaponManifest, String> {
    let tokens = tokenize_key_values(text)?;
    let find_value = |name: &str| -> Result<f32, String> {
        let index = tokens
            .iter()
            .position(|token| token.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("weapon_ak47.txt is missing {name}"))?;
        let value = tokens
            .get(index + 1)
            .ok_or_else(|| format!("weapon_ak47.txt has no value for {name}"))?;
        value
            .parse::<f32>()
            .map_err(|error| format!("Invalid {name} value {value:?}: {error}"))
    };

    let damage = find_value("Damage")?;
    let cycle_time_index = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("CycleTime"))
        .ok_or_else(|| "weapon_ak47.txt is missing CycleTime".to_string())?;
    let cycle_time_text = tokens
        .get(cycle_time_index + 1)
        .ok_or_else(|| "weapon_ak47.txt has no value for CycleTime".to_string())?;
    let cycle_time = cycle_time_text
        .parse::<f64>()
        .map_err(|error| format!("Invalid CycleTime value {cycle_time_text:?}: {error}"))?;
    let max_player_speed = find_value("MaxPlayerSpeed")?;
    let inaccuracy_move = find_value("InaccuracyMove")?;
    if !damage.is_finite()
        || damage < 1.0
        || damage > u16::MAX as f32
        || damage.fract() != 0.0
        || !cycle_time.is_finite()
        || cycle_time <= 0.0
        || !max_player_speed.is_finite()
        || max_player_speed <= 0.0
        || !inaccuracy_move.is_finite()
        || inaccuracy_move < 0.0
    {
        return Err("weapon_ak47.txt contains invalid combat values".to_string());
    }
    Ok(WeaponManifest {
        damage: damage as u16,
        cycle_time,
        max_player_speed,
        inaccuracy_move,
    })
}

#[derive(Component)]
struct TracerLifetime(Timer);

fn setup_tracer_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(TracerAssets {
        mesh: meshes.add(Cuboid::new(0.018, 0.018, 1.0)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.78, 0.18),
            emissive: Color::srgb(1.0, 0.56, 0.08).into(),
            unlit: true,
            ..default()
        }),
    });
}

fn recover_weapon(
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    console: Res<ConsoleState>,
    mut players: Query<(&mut WeaponState, &mut Player)>,
) {
    if console.open || buttons.pressed(MouseButton::Left) {
        return;
    }

    let recovery = time.delta_seconds();
    for (mut weapon, mut player) in &mut players {
        weapon.recoil_index = (weapon.recoil_index - RECOIL_RECOVERY * recovery).max(0.0);
        weapon.current_inaccuracy =
            (weapon.current_inaccuracy - INACCURACY_RECOVERY * recovery).max(0.0);
        player.view_punch *= (-VIEW_PUNCH_RECOVERY * recovery).exp();
        if player.view_punch.length_squared() < 0.000001 {
            player.view_punch = Vec2::ZERO;
        }
    }
}

fn fire_hitscan(
    mut commands: Commands,
    mut sound_cues: EventWriter<SoundCue>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    console: Res<ConsoleState>,
    convars: Res<ConVars>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    tracer_assets: Res<TracerAssets>,
    mut players: Query<(&mut Transform, &mut Player, &mut WeaponState), Without<Solid>>,
    hitboxes: Query<(&GlobalTransform, &Solid, Option<&Hitbox>, Option<&Parent>), Without<Player>>,
    mut targets: Query<&mut Target>,
) {
    if console.open {
        return;
    }
    let now = time.elapsed_seconds_f64();
    let Ok(window) = windows.get_single() else {
        return;
    };
    if window.cursor.grab_mode != CursorGrabMode::Locked {
        return;
    }

    let Ok((player_transform, mut player, mut weapon)) = players.get_single_mut() else {
        return;
    };

    weapon.update_reload(now);
    if keys.just_pressed(KeyCode::KeyR) {
        weapon.start_reload(now);
    }
    if !buttons.pressed(MouseButton::Left) || !weapon.try_fire(now) {
        return;
    }
    sound_cues.send(SoundCue::WeaponFire);

    let camera_origin = player_transform.translation;
    let shot_rotation = view_rotation(&player);
    let camera_forward = shot_rotation * Vec3::NEG_Z;
    let horizontal_speed = Vec2::new(player.velocity.x, player.velocity.z).length();
    let movement_inaccuracy =
        (horizontal_speed / convars.max_speed).clamp(0.0, 1.0) * weapon.inaccuracy_move;
    let airborne_inaccuracy = if player.grounded {
        0.0
    } else {
        AIRBORNE_INACCURACY
    };
    let spread = calculate_spread(
        movement_inaccuracy,
        airborne_inaccuracy,
        weapon.current_inaccuracy,
        player.crouch_fraction,
        player.walking,
    )
    .clamp(0.0, MAX_INACCURACY);
    let direction = apply_spread(camera_forward, spread, &mut weapon);

    let mut intersections = Vec::new();
    for (transform, solid, hitbox, parent) in &hitboxes {
        if !solid.blocks_shots {
            continue;
        }

        let position = transform.translation();
        let min = position - solid.half_extents;
        let max = position + solid.half_extents;
        if let Some((entry, exit)) = ray_aabb_interval(camera_origin, direction, min, max) {
            if entry <= MAX_SHOT_RANGE {
                let (target, hitgroup) = match (hitbox, parent) {
                    (Some(hitbox), Some(parent)) => (Some(parent.get()), Some(hitbox.group)),
                    _ => (None, None),
                };
                intersections.push(Intersection {
                    entry,
                    exit: exit.min(MAX_SHOT_RANGE),
                    resistance: solid.surface.penetration_resistance.max(0.0),
                    target,
                    hitgroup,
                });
            }
        }
    }
    intersections.sort_by(|a, b| a.entry.total_cmp(&b.entry));

    let mut damage = weapon.damage as f32;
    let mut penetration = PENETRATION_POWER;
    let mut tracer_end = camera_origin + direction * MAX_SHOT_RANGE;
    let mut hit_target = None;

    for intersection in intersections {
        if let (Some(target), Some(hitgroup)) = (intersection.target, intersection.hitgroup) {
            hit_target = Some((target, hitgroup, scaled_damage(damage, hitgroup).max(1)));
            tracer_end = camera_origin + direction * intersection.entry;
            break;
        }

        let thickness = (intersection.exit - intersection.entry).max(0.0);
        let penetration_cost = thickness * intersection.resistance;
        if penetration_cost <= 0.0 {
            tracer_end = camera_origin + direction * intersection.exit;
            continue;
        }
        if penetration_cost >= penetration {
            let distance_inside = penetration / intersection.resistance;
            tracer_end = camera_origin + direction * (intersection.entry + distance_inside);
            break;
        }

        (damage, penetration) = attenuate_penetration(damage, penetration, penetration_cost);
        tracer_end = camera_origin + direction * intersection.exit;
    }

    let muzzle = camera_origin + shot_rotation * Vec3::new(0.28, -0.23, -0.72);
    spawn_tracer(&mut commands, &tracer_assets, muzzle, tracer_end);

    if let Some((entity, hitgroup, damage)) = hit_target {
        if let Ok(mut target) = targets.get_mut(entity) {
            target.take_damage(damage);
            if target.is_destroyed() {
                if hitgroup == HitGroup::Head {
                    info!("[HEADSHOT] Target Destroyed");
                    sound_cues.send(SoundCue::Headshot);
                }
                commands.entity(entity).despawn_recursive();
            }
        }
    }

    weapon.recoil_index += 1.0;
    weapon.current_inaccuracy = (weapon.current_inaccuracy + SHOT_INACCURACY).min(MAX_INACCURACY);
    let recoil = recoil_step(weapon.recoil_index);
    player.view_punch += recoil;
    player.view_punch.x = player.view_punch.x.clamp(-0.3, 0.3);
    player.view_punch.y = player.view_punch.y.clamp(-0.3, 0.3);
}

fn apply_spread(forward: Vec3, spread: f32, weapon: &mut WeaponState) -> Vec3 {
    let forward = forward.normalize_or_zero();
    if spread <= 0.0 || forward == Vec3::ZERO {
        return forward;
    }

    let right = forward.cross(Vec3::Y).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let radius = weapon.random_unit().sqrt() * spread;
    let angle = weapon.random_unit() * std::f32::consts::TAU;
    let (sin_angle, cos_angle) = angle.sin_cos();

    (forward + right * (radius * cos_angle) + up * (radius * sin_angle)).normalize_or_zero()
}

fn view_rotation(player: &Player) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        player.yaw + player.view_punch.y,
        player.pitch + player.view_punch.x,
        0.0,
    )
}

fn apply_view_rotation(console: Res<ConsoleState>, mut players: Query<(&mut Transform, &Player)>) {
    if console.open {
        return;
    }
    for (mut transform, player) in &mut players {
        transform.rotation = view_rotation(player);
    }
}

fn recoil_step(recoil_index: f32) -> Vec2 {
    let index = recoil_index.clamp(1.0, 32.0);
    let pitch_kick = 0.0008 + index * 0.00002;
    let yaw_kick = (index * 1.618).sin() * 0.00045;
    Vec2::new(pitch_kick, yaw_kick)
}

fn calculate_spread(
    movement_inaccuracy: f32,
    airborne_inaccuracy: f32,
    base_inaccuracy: f32,
    crouch_fraction: f32,
    walking: bool,
) -> f32 {
    let crouch = crouch_fraction.clamp(0.0, 1.0);
    let movement_scale = if crouch > 0.0 {
        1.0 - 0.75 * crouch
    } else if walking {
        0.55
    } else {
        1.0
    };
    let base_scale = if crouch > 0.0 {
        1.0 - 0.25 * crouch
    } else if walking {
        0.85
    } else {
        1.0
    };
    movement_inaccuracy * movement_scale + airborne_inaccuracy + base_inaccuracy * base_scale
}

fn attenuate_penetration(damage: f32, power: f32, material_cost: f32) -> (f32, f32) {
    let remaining_power = (power - material_cost).max(0.0);
    let fraction = if power > 0.0 {
        remaining_power / power
    } else {
        0.0
    };
    (damage * fraction, remaining_power)
}

fn spawn_tracer(commands: &mut Commands, assets: &TracerAssets, start: Vec3, end: Vec3) {
    let segment = end - start;
    let length = segment.length();
    if length <= f32::EPSILON {
        return;
    }

    let direction = segment / length;
    commands.spawn((
        PbrBundle {
            mesh: assets.mesh.clone(),
            material: assets.material.clone(),
            transform: Transform {
                translation: (start + end) * 0.5,
                rotation: Quat::from_rotation_arc(Vec3::Z, direction),
                scale: Vec3::new(1.0, 1.0, length),
            },
            ..default()
        },
        TracerLifetime(Timer::from_seconds(TRACER_LIFETIME, TimerMode::Once)),
    ));
}

fn tick_tracers(
    mut commands: Commands,
    time: Res<Time>,
    mut tracers: Query<(Entity, &mut TracerLifetime)>,
) {
    for (entity, mut lifetime) in &mut tracers {
        lifetime.0.tick(time.delta());
        if lifetime.0.finished() {
            commands.entity(entity).despawn_recursive();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Intersection {
    entry: f32,
    exit: f32,
    resistance: f32,
    target: Option<Entity>,
    hitgroup: Option<HitGroup>,
}

fn ray_aabb_interval(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<(f32, f32)> {
    if direction.length_squared() <= f32::EPSILON {
        return None;
    }

    let mut near = f32::NEG_INFINITY;
    let mut far = f32::INFINITY;

    for axis in 0..3 {
        let origin_axis = origin[axis];
        let direction_axis = direction[axis];
        if direction_axis.abs() <= f32::EPSILON {
            if origin_axis < min[axis] || origin_axis > max[axis] {
                return None;
            }
            continue;
        }

        let first = (min[axis] - origin_axis) / direction_axis;
        let second = (max[axis] - origin_axis) / direction_axis;
        near = near.max(first.min(second));
        far = far.min(first.max(second));
        if near > far {
            return None;
        }
    }

    if far < 0.0 {
        None
    } else if near >= 0.0 {
        Some((near, far))
    } else {
        Some((0.0, far))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hitscan_interval_returns_entry_and_exit_distances() {
        let interval = ray_aabb_interval(
            Vec3::ZERO,
            Vec3::NEG_Z,
            Vec3::new(-1.0, -1.0, -5.0),
            Vec3::new(1.0, 1.0, -4.0),
        );
        assert_eq!(interval, Some((4.0, 5.0)));
    }

    #[test]
    fn hitscan_misses_boxes_outside_the_ray() {
        let distance = ray_aabb_interval(
            Vec3::ZERO,
            Vec3::NEG_Z,
            Vec3::new(2.0, -1.0, -5.0),
            Vec3::new(3.0, 1.0, -4.0),
        );
        assert_eq!(distance, None);
    }

    #[test]
    fn hitscan_handles_parallel_axes_and_origins_inside_a_box() {
        let interval =
            ray_aabb_interval(Vec3::ZERO, Vec3::NEG_Z, Vec3::splat(-1.0), Vec3::splat(1.0));
        assert_eq!(interval, Some((0.0, 1.0)));
    }

    #[test]
    fn firing_obeys_interval_and_consumes_one_round() {
        let mut weapon = WeaponState::default();
        assert!(weapon.try_fire(0.0));
        assert_eq!(weapon.ammunition, MAGAZINE_SIZE - 1);
        assert!(!weapon.try_fire(SHOT_INTERVAL - 0.001));
        assert!(weapon.try_fire(SHOT_INTERVAL));
        assert_eq!(weapon.ammunition, MAGAZINE_SIZE - 2);
    }

    #[test]
    fn manifest_cycle_time_controls_fire_interval() {
        let mut weapon = WeaponState {
            cycle_time: 0.25,
            ..default()
        };
        assert!(weapon.try_fire(0.0));
        assert!(!weapon.try_fire(0.249));
        assert!(weapon.try_fire(0.25));
    }

    #[test]
    fn empty_magazine_starts_reload_and_refills_at_completion() {
        let mut weapon = WeaponState {
            ammunition: 0,
            ..default()
        };

        assert!(!weapon.try_fire(1.0));
        assert!(weapon.is_reloading());
        assert!(!weapon.try_fire(1.0 + RELOAD_DURATION - 0.001));
        assert!(weapon.try_fire(1.0 + RELOAD_DURATION));
        assert_eq!(weapon.ammunition, MAGAZINE_SIZE - 1);
        assert!(!weapon.is_reloading());
    }

    #[test]
    fn ak47_manifest_parser_loads_dynamic_combat_values() {
        let manifest = parse_weapon_manifest(
            r#"
            "WeaponData"
            {
                "Damage" "36"
                "CycleTime" "0.1"
                "MaxPlayerSpeed" "215"
                "InaccuracyMove" "0.04"
            }
            "#,
        )
        .unwrap();
        assert_eq!(
            manifest,
            WeaponManifest {
                damage: 36,
                cycle_time: 0.1,
                max_player_speed: 215.0,
                inaccuracy_move: 0.04,
            }
        );
    }

    #[test]
    fn ak47_manifest_rejects_missing_or_invalid_values() {
        assert!(parse_weapon_manifest(r#""WeaponData" { "Damage" "36" }"#).is_err());
        assert!(
            parse_weapon_manifest(
                r#""WeaponData" { "Damage" "36" "CycleTime" "0" "MaxPlayerSpeed" "215" "InaccuracyMove" "0.04" }"#
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_reload_preserves_ammunition_until_finished() {
        let mut weapon = WeaponState::default();
        assert!(weapon.try_fire(0.0));
        weapon.start_reload(0.2);
        assert_eq!(weapon.ammunition, MAGAZINE_SIZE - 1);
        weapon.update_reload(0.2 + RELOAD_DURATION);
        assert_eq!(weapon.ammunition, MAGAZINE_SIZE);
    }

    #[test]
    fn range_filter_rejects_hits_beyond_weapon_limit() {
        let interval = ray_aabb_interval(
            Vec3::ZERO,
            Vec3::NEG_Z,
            Vec3::new(-1.0, -1.0, -MAX_SHOT_RANGE - 2.0),
            Vec3::new(1.0, 1.0, -MAX_SHOT_RANGE - 1.0),
        );
        assert!(interval.is_some_and(|(entry, _)| entry > MAX_SHOT_RANGE));
    }

    #[test]
    fn spread_is_deterministic_and_deviates_from_center_when_enabled() {
        let mut first = WeaponState::default();
        let mut second = WeaponState::default();
        let forward = Vec3::NEG_Z;
        let first_direction = apply_spread(forward, 0.05, &mut first);
        let second_direction = apply_spread(forward, 0.05, &mut second);

        assert!(first_direction.distance(forward) > 0.0);
        assert_eq!(first_direction, second_direction);
        assert!((first_direction.length() - 1.0).abs() < 0.0001);
    }

    #[test]
    fn recoil_step_has_upward_component_and_varied_lateral_component() {
        let step = recoil_step(5.0);
        assert!(step.x > 0.0);
        assert!(step.y.abs() > 0.0);
        assert!(recoil_step(12.0).x > step.x);
    }

    #[test]
    fn view_rotation_adds_recoil_to_base_angles_without_mutating_them() {
        let mut player = Player::default();
        player.yaw = 0.3;
        player.pitch = -0.2;
        player.view_punch = Vec2::new(0.1, -0.05);
        let rotation = view_rotation(&player);
        let expected = Quat::from_euler(EulerRot::YXZ, 0.25, -0.1, 0.0);
        assert!(rotation.dot(expected).abs() > 0.99999);
        assert_eq!(player.yaw, 0.3);
        assert_eq!(player.pitch, -0.2);
    }

    #[test]
    fn full_crouch_reduces_base_inaccuracy_by_one_quarter() {
        assert!((calculate_spread(0.0, 0.0, 0.08, 1.0, false) - 0.06).abs() < 0.0001);
        assert!((calculate_spread(0.0, 0.0, 0.08, 0.0, false) - 0.08).abs() < 0.0001);
    }

    #[test]
    fn walking_spread_is_between_running_and_full_crouch() {
        let running = calculate_spread(0.018, 0.0, 0.02, 0.0, false);
        let walking = calculate_spread(0.018, 0.0, 0.02, 0.0, true);
        let crouching = calculate_spread(0.018, 0.0, 0.02, 1.0, false);
        assert!(running > walking);
        assert!(walking > crouching);
    }

    #[test]
    fn thicker_or_denser_materials_reduce_damage_and_penetration() {
        let (thin_damage, thin_power) = attenuate_penetration(34.0, 3.5, 0.25);
        let (thick_damage, thick_power) = attenuate_penetration(34.0, 3.5, 1.0);
        assert!(thin_damage > thick_damage);
        assert!(thin_power > thick_power);
    }
}
