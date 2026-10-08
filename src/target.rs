use bevy::prelude::*;

use crate::environment::{DemoEnvironment, Solid, SurfacePhysics};

pub(super) const SHOT_DAMAGE: u16 = 34;

#[derive(Component)]
pub(super) struct Target {
    health: u16,
}

impl Target {
    pub(super) fn take_damage(&mut self, damage: u16) {
        self.health = self.health.saturating_sub(damage);
    }

    pub(super) fn is_destroyed(&self) -> bool {
        self.health == 0
    }

    #[cfg(test)]
    fn health(&self) -> u16 {
        self.health
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HitGroup {
    Head,
    Chest,
    Stomach,
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
}

impl HitGroup {
    pub(super) fn damage_multiplier(self) -> f32 {
        match self {
            Self::Head => 4.0,
            Self::Stomach => 1.25,
            Self::Chest | Self::LeftArm | Self::RightArm => 1.0,
            Self::LeftLeg | Self::RightLeg => 0.75,
        }
    }
}

#[derive(Component)]
pub(super) struct Hitbox {
    pub(super) group: HitGroup,
}

pub(super) fn spawn_target(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    position: Vec3,
) {
    commands
        .spawn((
            TransformBundle::from_transform(Transform::from_translation(position)),
            VisibilityBundle::default(),
            Target { health: 100 },
            DemoEnvironment,
        ))
        .with_children(|target| {
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::Head,
                Vec3::new(0.0, 0.79, 0.0),
                Vec3::new(0.32, 0.30, 0.30),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::Chest,
                Vec3::new(0.0, 0.37, 0.0),
                Vec3::new(0.48, 0.48, 0.30),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::Stomach,
                Vec3::new(0.0, -0.04, 0.0),
                Vec3::new(0.40, 0.32, 0.28),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::LeftArm,
                Vec3::new(-0.34, 0.33, 0.0),
                Vec3::new(0.20, 0.42, 0.26),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::RightArm,
                Vec3::new(0.34, 0.33, 0.0),
                Vec3::new(0.20, 0.42, 0.26),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::LeftLeg,
                Vec3::new(-0.12, -0.57, 0.0),
                Vec3::new(0.20, 0.70, 0.28),
            );
            spawn_hitbox(
                target,
                meshes,
                &material,
                HitGroup::RightLeg,
                Vec3::new(0.12, -0.57, 0.0),
                Vec3::new(0.20, 0.70, 0.28),
            );
        });
}

fn spawn_hitbox(
    target: &mut ChildBuilder,
    meshes: &mut Assets<Mesh>,
    material: &Handle<StandardMaterial>,
    group: HitGroup,
    center: Vec3,
    size: Vec3,
) {
    target.spawn((
        PbrBundle {
            mesh: meshes.add(Cuboid::new(size.x, size.y, size.z)),
            material: material.clone(),
            transform: Transform::from_translation(center),
            ..default()
        },
        Hitbox { group },
        Solid {
            half_extents: size * 0.5,
            surface: SurfacePhysics {
                friction: 1.0,
                jump_factor: 1.0,
                penetration_resistance: 0.0,
                surface_type: crate::environment::SurfaceType::Concrete,
            },
            blocks_shots: true,
            walkable_plane: None,
        },
    ));
}

pub(super) fn scaled_damage(base_damage: f32, group: HitGroup) -> u16 {
    (base_damage * group.damage_multiplier()).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_damage_uses_hitgroup_multipliers() {
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Head), 136);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Stomach), 43);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Chest), 34);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::LeftArm), 34);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::RightArm), 34);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::LeftLeg), 26);
        assert_eq!(scaled_damage(SHOT_DAMAGE as f32, HitGroup::RightLeg), 26);
    }

    #[test]
    fn three_body_hits_deplete_target_health_without_underflow() {
        let mut target = Target { health: 100 };
        target.take_damage(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Chest));
        assert_eq!(target.health(), 66);
        target.take_damage(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Chest));
        assert_eq!(target.health(), 32);
        target.take_damage(scaled_damage(SHOT_DAMAGE as f32, HitGroup::Chest));
        assert!(target.is_destroyed());
        target.take_damage(SHOT_DAMAGE);
        assert_eq!(target.health(), 0);
    }
}
