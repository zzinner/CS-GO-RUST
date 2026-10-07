use bevy::prelude::*;

use crate::assets_loader::AppState;
use crate::target::spawn_target;

pub(super) struct EnvironmentPlugin;

impl Plugin for EnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::srgb(0.47, 0.62, 0.69)))
            .add_systems(OnEnter(AppState::InGame), setup_environment);
    }
}

#[derive(Component)]
pub(super) struct Solid {
    pub(super) half_extents: Vec3,
    pub(super) surface: SurfacePhysics,
    pub(super) blocks_shots: bool,
    pub(super) walkable_plane: Option<WalkablePlane>,
}

#[derive(Clone, Copy)]
pub(super) struct WalkablePlane {
    pub(super) point: Vec3,
    pub(super) normal: Vec3,
    pub(super) half_bounds: Vec2,
}

impl WalkablePlane {
    pub(super) fn height_at(self, x: f32, z: f32) -> Option<f32> {
        if self.normal.y < std::f32::consts::FRAC_1_SQRT_2
            || (x - self.point.x).abs() > self.half_bounds.x
            || (z - self.point.z).abs() > self.half_bounds.y
        {
            return None;
        }

        Some(
            self.point.y
                - (self.normal.x * (x - self.point.x) + self.normal.z * (z - self.point.z))
                    / self.normal.y,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SurfaceType {
    Concrete,
    Metal,
    Wood,
}

#[derive(Clone, Copy)]
pub(super) struct SurfacePhysics {
    pub(super) friction: f32,
    pub(super) jump_factor: f32,
    pub(super) penetration_resistance: f32,
    pub(super) surface_type: SurfaceType,
}

impl SurfacePhysics {
    const CONCRETE: Self = Self {
        friction: 0.85,
        jump_factor: 1.0,
        penetration_resistance: 1.1,
        surface_type: SurfaceType::Concrete,
    };
    const PAINTED_METAL: Self = Self {
        friction: 0.65,
        jump_factor: 1.0,
        penetration_resistance: 0.7,
        surface_type: SurfaceType::Metal,
    };
    const WOOD: Self = Self {
        friction: 0.95,
        jump_factor: 1.05,
        penetration_resistance: 0.25,
        surface_type: SurfaceType::Wood,
    };
}

fn setup_environment(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            illuminance: 12_000.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(-6.0, 12.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.78, 0.84, 0.9),
        brightness: 180.0,
    });

    let ground = materials.add(Color::srgb(0.34, 0.39, 0.34));
    let concrete = materials.add(Color::srgb(0.53, 0.55, 0.51));
    let blue = materials.add(Color::srgb(0.22, 0.39, 0.46));
    let ochre = materials.add(Color::srgb(0.68, 0.43, 0.24));
    let pale = materials.add(Color::srgb(0.72, 0.68, 0.56));
    let target_material = materials.add(Color::srgb(0.82, 0.16, 0.12));

    spawn_box(
        &mut commands,
        &mut meshes,
        ground,
        Vec3::new(0.0, -0.2, 0.0),
        Vec3::new(28.0, 0.4, 36.0),
        Some(SurfacePhysics::CONCRETE),
    );

    let wall = materials.add(Color::srgb(0.43, 0.47, 0.46));
    for (position, size) in [
        (Vec3::new(-14.0, 2.0, 0.0), Vec3::new(0.5, 4.0, 36.0)),
        (Vec3::new(14.0, 2.0, 0.0), Vec3::new(0.5, 4.0, 36.0)),
        (Vec3::new(0.0, 2.0, -18.0), Vec3::new(28.0, 4.0, 0.5)),
        (Vec3::new(0.0, 2.0, 18.0), Vec3::new(28.0, 4.0, 0.5)),
    ] {
        spawn_box(
            &mut commands,
            &mut meshes,
            wall.clone(),
            position,
            size,
            Some(SurfacePhysics::CONCRETE),
        );
    }

    for (position, size, material, surface) in [
        (
            Vec3::new(-4.5, 0.65, 3.0),
            Vec3::new(4.0, 1.3, 1.0),
            concrete.clone(),
            SurfacePhysics::CONCRETE,
        ),
        (
            Vec3::new(4.2, 1.0, -1.0),
            Vec3::new(1.4, 2.0, 4.0),
            blue.clone(),
            SurfacePhysics::PAINTED_METAL,
        ),
        (
            Vec3::new(-8.0, 1.2, -7.0),
            Vec3::new(3.0, 2.4, 2.0),
            ochre.clone(),
            SurfacePhysics::WOOD,
        ),
        (
            Vec3::new(7.0, 0.75, -9.0),
            Vec3::new(4.0, 1.5, 2.0),
            pale.clone(),
            SurfacePhysics::CONCRETE,
        ),
        (
            Vec3::new(0.0, 0.55, -13.0),
            Vec3::new(7.0, 1.1, 1.0),
            concrete.clone(),
            SurfacePhysics::CONCRETE,
        ),
        (
            Vec3::new(-8.0, 0.55, 8.0),
            Vec3::new(2.0, 1.1, 3.0),
            blue.clone(),
            SurfacePhysics::PAINTED_METAL,
        ),
        (
            Vec3::new(8.0, 0.55, 7.0),
            Vec3::new(2.0, 1.1, 3.0),
            ochre.clone(),
            SurfacePhysics::WOOD,
        ),
    ] {
        spawn_box(
            &mut commands,
            &mut meshes,
            material,
            position,
            size,
            Some(surface),
        );
    }

    spawn_ramp(
        &mut commands,
        &mut meshes,
        concrete.clone(),
        Vec3::new(9.0, 0.66, -2.0),
        4.5,
        5.0,
        1.5,
        SurfacePhysics::CONCRETE,
    );

    spawn_target(
        &mut commands,
        &mut meshes,
        target_material,
        Vec3::new(0.0, 0.95, -7.5),
    );
}

fn spawn_ramp(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    center: Vec3,
    width: f32,
    length: f32,
    rise: f32,
    surface: SurfacePhysics,
) {
    let angle = (rise / length).atan();
    let rotation = Quat::from_rotation_x(angle);
    let thickness = 0.18;
    let local_half_extents = Vec3::new(width * 0.5, thickness * 0.5, length * 0.5);
    let world_half_extents = Vec3::new(
        local_half_extents.x,
        local_half_extents.y * angle.cos().abs() + local_half_extents.z * angle.sin().abs(),
        local_half_extents.y * angle.sin().abs() + local_half_extents.z * angle.cos().abs(),
    );
    let normal = rotation * Vec3::Y;
    let transform = Transform {
        translation: center,
        rotation,
        ..default()
    };
    let mesh = meshes.add(Cuboid::new(width, thickness, length));

    commands.spawn((
        PbrBundle {
            mesh,
            material,
            transform,
            ..default()
        },
        Solid {
            half_extents: world_half_extents,
            surface,
            blocks_shots: true,
            walkable_plane: Some(WalkablePlane {
                point: center + normal * (thickness * 0.5),
                normal,
                half_bounds: Vec2::new(
                    width * 0.5,
                    (length * angle.cos() + thickness * angle.sin()) * 0.5,
                ),
            }),
        },
    ));
}

fn spawn_box(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    position: Vec3,
    size: Vec3,
    surface: Option<SurfacePhysics>,
) {
    let mut entity = commands.spawn(PbrBundle {
        mesh: meshes.add(Cuboid::new(size.x, size.y, size.z)),
        material,
        transform: Transform::from_translation(position),
        ..default()
    });

    if let Some(surface) = surface {
        entity.insert(Solid {
            half_extents: size * 0.5,
            surface,
            blocks_shots: true,
            walkable_plane: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walkable_plane_height_changes_with_horizontal_position() {
        let plane = WalkablePlane {
            point: Vec3::new(0.0, 1.0, 0.0),
            normal: Vec3::new(0.0, 0.8944272, 0.4472136),
            half_bounds: Vec2::splat(4.0),
        };

        let low = plane.height_at(0.0, 1.0).unwrap();
        let high = plane.height_at(0.0, -1.0).unwrap();
        assert!(high > low);
        assert!(plane.height_at(0.0, 5.0).is_none());
    }

    #[test]
    fn plane_steeper_than_walkable_limit_is_rejected() {
        let plane = WalkablePlane {
            point: Vec3::ZERO,
            normal: Vec3::new(0.0, 0.6, 0.8),
            half_bounds: Vec2::splat(4.0),
        };
        assert!(plane.height_at(0.0, 0.0).is_none());
    }
}
