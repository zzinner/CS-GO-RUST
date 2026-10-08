use bevy::{
    prelude::*,
    render::{
        mesh::{Indices, PrimitiveTopology},
        render_asset::RenderAssetUsages,
    },
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
};

use crate::{
    assets_loader::{
        AppState, BSP_HEADER_SIZE, SourceAssetManager, tokenize_key_values, validate_bsp_header,
    },
    environment::DemoEnvironment,
    movement::{PLAYER_EYE_HEIGHT_STANDING, Player, SOURCE_WORLD_SCALE},
};

const LUMP_ENTITIES: usize = 0;
const LUMP_VERTEXES: usize = 3;
const LUMP_FACES: usize = 7;
const LUMP_EDGES: usize = 12;
const LUMP_SURFEDGES: usize = 13;
const BSP_FACE_SIZE: usize = 56;
const MAX_BSP_VERTICES: usize = 65_536;
const MAX_BSP_EDGES: usize = 256_000;
const MAX_BSP_SURFEDGES: usize = 512_000;
const MAX_BSP_FACES: usize = 65_536;

pub(super) struct BspMapPlugin;

impl Plugin for BspMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<MapLoadRequest>()
            .add_event::<MapLoadResult>()
            .init_resource::<PendingMapLoad>()
            .add_systems(
                Update,
                (poll_map_load, start_map_load)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Event)]
pub(super) struct MapLoadRequest {
    pub(super) map_name: String,
}

#[derive(Event)]
pub(super) struct MapLoadResult {
    pub(super) message: String,
}

#[derive(Resource, Default)]
struct PendingMapLoad(Option<(String, Task<Result<ParsedBspMap, String>>)>);

#[derive(Debug)]
struct ParsedBspMap {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
    spawn_position: Vec3,
    face_count: usize,
}

#[derive(Component)]
struct LoadedMapGeometry;

fn start_map_load(
    mut requests: EventReader<MapLoadRequest>,
    asset_manager: Res<SourceAssetManager>,
    mut pending: ResMut<PendingMapLoad>,
    mut results: EventWriter<MapLoadResult>,
) {
    for request in requests.read() {
        if pending.0.is_some() {
            results.send(MapLoadResult {
                message: format!(
                    "Map {} was not queued: another map is still loading",
                    request.map_name
                ),
            });
            continue;
        }

        let map_name = request.map_name.clone();
        let worker_name = map_name.clone();
        let manager = asset_manager.clone();
        let task = AsyncComputeTaskPool::get().spawn(async move {
            let bytes = manager.read_map(&worker_name)?;
            parse_bsp_map(&bytes)
        });
        pending.0 = Some((map_name.clone(), task));
    }
}

fn poll_map_load(
    mut commands: Commands,
    mut pending: ResMut<PendingMapLoad>,
    mut results: EventWriter<MapLoadResult>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut player_query: Query<(&mut Transform, &mut Player)>,
    old_maps: Query<Entity, With<LoadedMapGeometry>>,
    demo_environment: Query<Entity, With<DemoEnvironment>>,
) {
    let (map_name, result) = {
        let Some((map_name, task)) = pending.0.as_mut() else {
            return;
        };
        let Some(result) = future::block_on(future::poll_once(task)) else {
            return;
        };
        (map_name.clone(), result)
    };
    pending.0 = None;

    let map = match result {
        Ok(map) => map,
        Err(error) => {
            results.send(MapLoadResult {
                message: format!("Failed to load map {map_name}: {error}"),
            });
            return;
        }
    };

    let face_count = map.face_count;
    let vertex_count = map.positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, map.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, map.normals);
    mesh.insert_indices(Indices::U32(map.indices));
    let mesh_handle = meshes.add(mesh);
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.48, 0.49, 0.46),
        cull_mode: None,
        ..default()
    });

    for entity in &old_maps {
        commands.entity(entity).despawn_recursive();
    }
    for entity in &demo_environment {
        commands.entity(entity).despawn_recursive();
    }
    commands.spawn((
        PbrBundle {
            mesh: mesh_handle,
            material,
            ..default()
        },
        LoadedMapGeometry,
    ));

    match player_query.get_single_mut() {
        Ok((mut transform, mut player)) => {
            transform.translation = map.spawn_position;
            player.velocity = Vec3::ZERO;
            player.grounded = false;
            player.noclip = true;
            results.send(MapLoadResult {
                message: format!(
                    "Loaded {map_name}: {face_count} BSP faces, {vertex_count} vertices. Noclip is enabled; textures, displacements, props, and map collision are not imported yet."
                ),
            });
        }
        Err(error) => {
            results.send(MapLoadResult {
                message: format!(
                    "Loaded {map_name}, but could not move the player to its spawn: {error}"
                ),
            });
        }
    }
}

fn parse_bsp_map(bytes: &[u8]) -> Result<ParsedBspMap, String> {
    validate_bsp_header(bytes, bytes.len() as u64)?;

    let entities = bsp_lump(bytes, LUMP_ENTITIES, 1)?;
    let vertices = bsp_lump(bytes, LUMP_VERTEXES, 12)?;
    let faces = bsp_lump(bytes, LUMP_FACES, BSP_FACE_SIZE)?;
    let edges = bsp_lump(bytes, LUMP_EDGES, 4)?;
    let surfedges = bsp_lump(bytes, LUMP_SURFEDGES, 4)?;

    let vertex_count = vertices.len() / 12;
    let bsp_edge_count = edges.len() / 4;
    let surfedge_count = surfedges.len() / 4;
    let face_count = faces.len() / BSP_FACE_SIZE;
    if vertex_count > MAX_BSP_VERTICES
        || bsp_edge_count > MAX_BSP_EDGES
        || surfedge_count > MAX_BSP_SURFEDGES
        || face_count > MAX_BSP_FACES
    {
        return Err("BSP geometry exceeds Source's supported map limits".to_string());
    }
    if vertex_count == 0 || face_count == 0 {
        return Err("BSP has no renderable world geometry".to_string());
    }

    let mut positions = Vec::with_capacity(surfedge_count);
    let mut normals = Vec::with_capacity(surfedge_count);
    let mut indices = Vec::with_capacity(surfedge_count.saturating_mul(3));
    let mut rendered_faces = 0;
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    for face in faces.chunks_exact(BSP_FACE_SIZE) {
        let first_edge = read_i32(face, 4)?;
        let face_edge_count = read_i16(face, 8)?;
        if face_edge_count < 3 {
            continue;
        }
        let face_edge_count = face_edge_count as usize;
        let first_edge = usize::try_from(first_edge)
            .map_err(|_| "BSP face has a negative first surfedge".to_string())?;
        let end_edge = first_edge
            .checked_add(face_edge_count)
            .ok_or_else(|| "BSP face surfedge range overflow".to_string())?;
        if end_edge > surfedge_count {
            return Err("BSP face references surfedges outside the lump".to_string());
        }

        let side = face[2];
        if side > 1 {
            return Err(format!("BSP face has invalid side value {side}"));
        }
        let mut polygon = Vec::with_capacity(face_edge_count);
        for surfedge_index in first_edge..end_edge {
            let surfedge = read_i32(surfedges, surfedge_index * 4)?;
            let edge_index = surfedge.unsigned_abs() as usize;
            if edge_index >= bsp_edge_count {
                return Err("BSP face references an edge outside the lump".to_string());
            }
            let edge_offset = edge_index * 4;
            let vertex_slot = if surfedge >= 0 { 0 } else { 2 };
            let vertex_index = read_u16(edges, edge_offset + vertex_slot)? as usize;
            if vertex_index >= vertex_count {
                return Err("BSP edge references a vertex outside the lump".to_string());
            }
            let source = read_vertex(vertices, vertex_index)?;
            let position = Vec3::new(source.x, source.z, -source.y) * SOURCE_WORLD_SCALE;
            if !position.is_finite() {
                return Err("BSP contains a non-finite vertex position".to_string());
            }
            min = min.min(position);
            max = max.max(position);
            polygon.push(position);
        }
        if side == 1 {
            polygon.reverse();
        }

        let normal = (polygon[1] - polygon[0])
            .cross(polygon[2] - polygon[0])
            .try_normalize();
        let Some(normal) = normal else {
            continue;
        };
        let base_index = u32::try_from(positions.len())
            .map_err(|_| "BSP mesh exceeds the supported vertex count".to_string())?;
        positions.extend(polygon.iter().map(|point| point.to_array()));
        normals.extend(std::iter::repeat_n(normal.to_array(), polygon.len()));
        for corner in 1..polygon.len() - 1 {
            indices.extend([
                base_index,
                base_index + corner as u32,
                base_index + corner as u32 + 1,
            ]);
        }
        rendered_faces += 1;
    }

    if rendered_faces == 0 {
        return Err("BSP contains no non-degenerate faces to render".to_string());
    }

    let spawn_position = find_player_spawn(entities)?
        .unwrap_or_else(|| Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5))
        + Vec3::Y * PLAYER_EYE_HEIGHT_STANDING;

    Ok(ParsedBspMap {
        positions,
        normals,
        indices,
        spawn_position,
        face_count: rendered_faces,
    })
}

fn bsp_lump(bytes: &[u8], lump_index: usize, record_size: usize) -> Result<&[u8], String> {
    let descriptor = 8 + lump_index * 16;
    if descriptor + 16 > BSP_HEADER_SIZE {
        return Err(format!("BSP lump index {lump_index} is out of range"));
    }
    let offset = read_i32(bytes, descriptor)?;
    let length = read_i32(bytes, descriptor + 4)?;
    let four_cc = read_u32(bytes, descriptor + 12)?;
    if offset < 0 || length < 0 {
        return Err(format!("BSP lump {lump_index} has a negative range"));
    }
    if four_cc != 0 {
        return Err(format!(
            "BSP lump {lump_index} is compressed and is not supported yet"
        ));
    }
    let start = offset as usize;
    let end = start
        .checked_add(length as usize)
        .ok_or_else(|| format!("BSP lump {lump_index} range overflow"))?;
    let lump = bytes
        .get(start..end)
        .ok_or_else(|| format!("BSP lump {lump_index} extends beyond the file"))?;
    if record_size > 1 && lump.len() % record_size != 0 {
        return Err(format!(
            "BSP lump {lump_index} size is not a multiple of {record_size}"
        ));
    }
    Ok(lump)
}

fn find_player_spawn(entities: &[u8]) -> Result<Option<Vec3>, String> {
    let entity_text = String::from_utf8_lossy(entities);
    let tokens = tokenize_key_values(entity_text.trim_end_matches('\0'))?;
    let mut best: Option<(u8, Vec3)> = None;
    let mut cursor = 0;

    while cursor < tokens.len() {
        if tokens[cursor] != "{" {
            cursor += 1;
            continue;
        }
        cursor += 1;
        let mut depth = 1;
        let mut class_name = None;
        let mut origin = None;
        while cursor < tokens.len() && depth > 0 {
            match tokens[cursor].as_str() {
                "{" => {
                    depth += 1;
                    cursor += 1;
                }
                "}" => {
                    depth -= 1;
                    cursor += 1;
                }
                _ if depth == 1 && cursor + 1 < tokens.len() => {
                    match tokens[cursor].as_str() {
                        "classname" => class_name = Some(tokens[cursor + 1].as_str()),
                        "origin" => origin = Some(tokens[cursor + 1].as_str()),
                        _ => {}
                    }
                    cursor += 2;
                }
                _ => cursor += 1,
            }
        }
        let Some((priority, origin)) = class_name
            .and_then(spawn_priority)
            .zip(origin.and_then(parse_origin))
        else {
            continue;
        };
        if best.is_none_or(|(best_priority, _)| priority < best_priority) {
            let source_position = Vec3::from_array(origin);
            let world_position =
                Vec3::new(source_position.x, source_position.z, -source_position.y)
                    * SOURCE_WORLD_SCALE;
            best = Some((priority, world_position));
        }
    }

    Ok(best.map(|(_, position)| position))
}

fn spawn_priority(class_name: &str) -> Option<u8> {
    match class_name {
        "info_player_counterterrorist" => Some(0),
        "info_player_terrorist" => Some(1),
        "info_player_start" => Some(2),
        _ => None,
    }
}

fn parse_origin(origin: &str) -> Option<[f32; 3]> {
    let values: Vec<f32> = origin
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<_, _>>()
        .ok()?;
    let [x, y, z] = values.as_slice() else {
        return None;
    };
    (x.is_finite() && y.is_finite() && z.is_finite()).then_some([*x, *y, *z])
}

fn read_vertex(bytes: &[u8], index: usize) -> Result<Vec3, String> {
    let offset = index
        .checked_mul(12)
        .ok_or_else(|| "BSP vertex offset overflow".to_string())?;
    Ok(Vec3::new(
        read_f32(bytes, offset)?,
        read_f32(bytes, offset + 4)?,
        read_f32(bytes, offset + 8)?,
    ))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "BSP field offset overflow".to_string())?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| "BSP field is truncated".to_string())?;
    Ok(i16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "BSP field offset overflow".to_string())?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| "BSP field is truncated".to_string())?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "BSP field offset overflow".to_string())?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| "BSP field is truncated".to_string())?;
    Ok(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "BSP field offset overflow".to_string())?;
    let raw = bytes
        .get(offset..end)
        .ok_or_else(|| "BSP field is truncated".to_string())?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, String> {
    Ok(f32::from_bits(read_u32(bytes, offset)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bsp_parser_triangulates_world_faces_and_uses_a_player_spawn() {
        let bytes = triangle_bsp(2);
        let parsed = parse_bsp_map(&bytes).unwrap();

        assert_eq!(parsed.face_count, 1);
        assert_eq!(parsed.indices, vec![0, 1, 2]);
        assert!((parsed.positions[1][0] - 2.08).abs() < 0.001);
        assert!((parsed.spawn_position.x - 0.2496).abs() < 0.001);
        assert!((parsed.spawn_position.y - (0.9984 + PLAYER_EYE_HEIGHT_STANDING)).abs() < 0.001);
        assert!((parsed.spawn_position.z + 0.4992).abs() < 0.001);
    }

    #[test]
    fn bsp_parser_rejects_faces_with_out_of_range_edges() {
        let error = parse_bsp_map(&triangle_bsp(3)).unwrap_err();
        assert!(error.contains("references an edge outside the lump"));
    }

    #[test]
    fn entity_spawn_selection_prefers_counterterrorist_spawns() {
        let entities = br#"
            { "classname" "info_player_terrorist" "origin" "100 0 0" }
            { "classname" "info_player_counterterrorist" "origin" "0 100 0" }
        "#;
        let spawn = find_player_spawn(entities).unwrap().unwrap();
        assert_eq!(spawn, Vec3::new(0.0, 0.0, -2.08));
    }

    fn triangle_bsp(last_surfedge: i32) -> Vec<u8> {
        let mut bytes = vec![0; BSP_HEADER_SIZE];
        bytes[0..4].copy_from_slice(b"VBSP");
        bytes[4..8].copy_from_slice(&21_u32.to_le_bytes());

        let mut vertices = Vec::new();
        for point in [[0.0_f32, 0.0, 0.0], [100.0, 0.0, 0.0], [100.0, 100.0, 0.0]] {
            for coordinate in point {
                vertices.extend_from_slice(&coordinate.to_le_bytes());
            }
        }
        let mut face = vec![0; BSP_FACE_SIZE];
        face[4..8].copy_from_slice(&0_i32.to_le_bytes());
        face[8..10].copy_from_slice(&3_i16.to_le_bytes());
        let mut edges = Vec::new();
        for edge in [[0_u16, 1_u16], [1, 2], [2, 0]] {
            edges.extend_from_slice(&edge[0].to_le_bytes());
            edges.extend_from_slice(&edge[1].to_le_bytes());
        }
        let surfedges = [0_i32, 1_i32, last_surfedge]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect::<Vec<_>>();
        let entities = br#"{ "classname" "info_player_counterterrorist" "origin" "12 24 48" }"#;
        for (lump_index, lump) in [
            (LUMP_ENTITIES, entities.as_slice()),
            (LUMP_VERTEXES, vertices.as_slice()),
            (LUMP_FACES, face.as_slice()),
            (LUMP_EDGES, edges.as_slice()),
            (LUMP_SURFEDGES, surfedges.as_slice()),
        ] {
            let offset = bytes.len();
            bytes.extend_from_slice(lump);
            let descriptor = 8 + lump_index * 16;
            bytes[descriptor..descriptor + 4].copy_from_slice(&(offset as i32).to_le_bytes());
            bytes[descriptor + 4..descriptor + 8]
                .copy_from_slice(&(lump.len() as i32).to_le_bytes());
        }
        bytes
    }
}
