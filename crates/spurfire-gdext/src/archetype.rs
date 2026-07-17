//! Immutable M0.5 horse archetype attribute rows.

/// The three per-match horse sidegrades exposed to Godot by numeric ID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(i64)]
pub enum HorseArchetype {
    Courser = 0,
    Warhorse = 1,
    #[default]
    Mustang = 2,
}

impl HorseArchetype {
    pub const ALL: [Self; 3] = [Self::Courser, Self::Warhorse, Self::Mustang];

    #[must_use]
    pub const fn id(self) -> i64 {
        self as i64
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Courser => "Courser",
            Self::Warhorse => "Warhorse",
            Self::Mustang => "Mustang",
        }
    }

    /// Return the process-lifetime immutable design row for this archetype.
    #[must_use]
    pub const fn stats(self) -> &'static HorseStats {
        match self {
            Self::Courser => &COURSER_STATS,
            Self::Warhorse => &WARHORSE_STATS,
            Self::Mustang => &MUSTANG_STATS,
        }
    }
}

impl TryFrom<i64> for HorseArchetype {
    type Error = InvalidArchetypeId;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Courser),
            1 => Ok(Self::Warhorse),
            2 => Ok(Self::Mustang),
            id => Err(InvalidArchetypeId(id)),
        }
    }
}

/// Rejected numeric archetype ID. Conversion never silently falls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidArchetypeId(pub i64);

/// Coarse mass band reserved for future collision and combat systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum HorseMass {
    Light = 0,
    Medium = 1,
    Heavy = 2,
}

impl HorseMass {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Heavy => "heavy",
        }
    }
}

/// Numeric design contract for one horse archetype.
///
/// Rows are private statics returned through [`HorseArchetype::stats`], so the
/// simulation and Godot adapter share one immutable source of truth. Combat
/// attributes are exposed now but deliberately have no M0.5 combat behavior.
#[derive(Debug, PartialEq)]
pub struct HorseStats {
    pub walk_mps: f64,
    pub trot_mps: f64,
    pub gallop_mps: f64,
    pub sprint_mps: f64,
    pub accel_0_to_gallop_s: f64,
    pub brake_deceleration_mps2: f64,
    pub turn_walk_deg_s: f64,
    pub turn_gallop_deg_s: f64,
    pub drift_deg_s: f64,
    pub drift_duration_s: f64,
    pub drift_speed_multiplier: f64,
    pub jump_apex_m: f64,
    pub jump_airtime_s: f64,
    pub terrain_scrub: f64,
    pub terrain_mud: f64,
    pub terrain_riverbed: f64,
    pub terrain_recovery_s: f64,
    pub max_vitality: f64,
    pub rider_vitality_multiple: f64,
    pub mass: HorseMass,
    pub stagger_threshold: f64,
    pub collision_sway_impulse: f64,
    pub hard_brake_slide_multiplier: f64,
    pub sidestep_mps: f64,
    pub sidestep_ramp_s: f64,
    pub sidestep_roll_degrees: f64,
    pub reverse_mps: f64,
    pub saddle_height_m: f64,
}

impl HorseStats {
    /// Constant-rate approximation of the specified zero-to-gallop profile.
    #[must_use]
    pub fn forward_acceleration_mps2(&self) -> f64 {
        self.gallop_mps / self.accel_0_to_gallop_s
    }

    /// Initial vertical velocity that reaches both the specified apex and airtime.
    #[must_use]
    pub fn jump_velocity_mps(&self) -> f64 {
        4.0 * self.jump_apex_m / self.jump_airtime_s
    }

    /// Downward acceleration paired with [`Self::jump_velocity_mps`].
    #[must_use]
    pub fn jump_gravity_mps2(&self) -> f64 {
        8.0 * self.jump_apex_m / self.jump_airtime_s.powi(2)
    }

    /// Design turn rate linearly tapered by forward speed from walk to gallop.
    #[must_use]
    pub fn turn_rate_deg_s(&self, forward_speed_mps: f64) -> f64 {
        let span = (self.gallop_mps - self.walk_mps).max(f64::EPSILON);
        let fraction = ((forward_speed_mps.abs() - self.walk_mps) / span).clamp(0.0, 1.0);
        self.turn_walk_deg_s + (self.turn_gallop_deg_s - self.turn_walk_deg_s) * fraction
    }
}

const SHARED_REVERSE_MPS: f64 = 1.5;
const SHARED_SADDLE_HEIGHT_M: f64 = 1.6;
const DRIFT_DURATION_S: f64 = 2.0;
const DRIFT_SPEED_MULTIPLIER: f64 = 0.70;

static COURSER_STATS: HorseStats = HorseStats {
    walk_mps: 2.0,
    trot_mps: 5.0,
    gallop_mps: 14.5,
    sprint_mps: 16.5,
    accel_0_to_gallop_s: 3.0,
    brake_deceleration_mps2: 6.0,
    turn_walk_deg_s: 140.0,
    turn_gallop_deg_s: 60.0,
    drift_deg_s: 120.0,
    drift_duration_s: DRIFT_DURATION_S,
    drift_speed_multiplier: DRIFT_SPEED_MULTIPLIER,
    jump_apex_m: 1.8,
    jump_airtime_s: 0.7,
    terrain_scrub: 0.90,
    terrain_mud: 0.70,
    terrain_riverbed: 0.95,
    terrain_recovery_s: 2.0,
    max_vitality: 200.0,
    rider_vitality_multiple: 2.0,
    mass: HorseMass::Light,
    stagger_threshold: 40.0,
    collision_sway_impulse: 0.8,
    hard_brake_slide_multiplier: 1.2,
    sidestep_mps: 1.0,
    sidestep_ramp_s: 0.25,
    sidestep_roll_degrees: 5.0,
    reverse_mps: SHARED_REVERSE_MPS,
    saddle_height_m: SHARED_SADDLE_HEIGHT_M,
};

static WARHORSE_STATS: HorseStats = HorseStats {
    walk_mps: 1.8,
    trot_mps: 4.2,
    gallop_mps: 12.0,
    sprint_mps: 13.5,
    accel_0_to_gallop_s: 5.0,
    brake_deceleration_mps2: 5.0,
    turn_walk_deg_s: 120.0,
    turn_gallop_deg_s: 45.0,
    drift_deg_s: 90.0,
    drift_duration_s: DRIFT_DURATION_S,
    drift_speed_multiplier: DRIFT_SPEED_MULTIPLIER,
    jump_apex_m: 1.2,
    jump_airtime_s: 0.5,
    terrain_scrub: 0.90,
    terrain_mud: 0.75,
    terrain_riverbed: 0.95,
    terrain_recovery_s: 2.5,
    max_vitality: 320.0,
    rider_vitality_multiple: 3.2,
    mass: HorseMass::Heavy,
    stagger_threshold: 90.0,
    collision_sway_impulse: 0.4,
    hard_brake_slide_multiplier: 1.0,
    sidestep_mps: 0.8,
    sidestep_ramp_s: 0.35,
    sidestep_roll_degrees: 4.0,
    reverse_mps: SHARED_REVERSE_MPS,
    saddle_height_m: SHARED_SADDLE_HEIGHT_M,
};

static MUSTANG_STATS: HorseStats = HorseStats {
    walk_mps: 1.9,
    trot_mps: 4.6,
    gallop_mps: 13.0,
    sprint_mps: 14.5,
    accel_0_to_gallop_s: 3.5,
    brake_deceleration_mps2: 5.5,
    turn_walk_deg_s: 150.0,
    turn_gallop_deg_s: 80.0,
    drift_deg_s: 150.0,
    drift_duration_s: DRIFT_DURATION_S,
    drift_speed_multiplier: DRIFT_SPEED_MULTIPLIER,
    jump_apex_m: 2.2,
    jump_airtime_s: 0.8,
    terrain_scrub: 0.95,
    terrain_mud: 0.80,
    terrain_riverbed: 0.95,
    terrain_recovery_s: 1.5,
    max_vitality: 250.0,
    rider_vitality_multiple: 2.5,
    mass: HorseMass::Medium,
    stagger_threshold: 60.0,
    collision_sway_impulse: 0.6,
    hard_brake_slide_multiplier: 1.0,
    sidestep_mps: 1.2,
    sidestep_ramp_s: 0.15,
    sidestep_roll_degrees: 6.0,
    reverse_mps: SHARED_REVERSE_MPS,
    saddle_height_m: SHARED_SADDLE_HEIGHT_M,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_locked_and_mustang_is_default() {
        assert_eq!(HorseArchetype::default(), HorseArchetype::Mustang);
        for archetype in HorseArchetype::ALL {
            assert_eq!(HorseArchetype::try_from(archetype.id()), Ok(archetype));
        }
        for invalid in [-1, 3, 7, i64::MAX] {
            assert_eq!(
                HorseArchetype::try_from(invalid),
                Err(InvalidArchetypeId(invalid))
            );
        }
    }

    #[test]
    fn rows_match_the_locked_design_values() {
        let courser = HorseArchetype::Courser.stats();
        assert_eq!(
            (
                courser.walk_mps,
                courser.trot_mps,
                courser.gallop_mps,
                courser.sprint_mps,
                courser.accel_0_to_gallop_s,
                courser.brake_deceleration_mps2,
            ),
            (2.0, 5.0, 14.5, 16.5, 3.0, 6.0)
        );
        assert_eq!(
            (
                courser.turn_walk_deg_s,
                courser.turn_gallop_deg_s,
                courser.jump_apex_m,
                courser.jump_airtime_s,
                courser.max_vitality,
                courser.stagger_threshold,
                courser.sidestep_mps,
                courser.sidestep_ramp_s,
            ),
            (140.0, 60.0, 1.8, 0.7, 200.0, 40.0, 1.0, 0.25)
        );

        let warhorse = HorseArchetype::Warhorse.stats();
        assert_eq!(
            (
                warhorse.walk_mps,
                warhorse.trot_mps,
                warhorse.gallop_mps,
                warhorse.sprint_mps,
                warhorse.accel_0_to_gallop_s,
                warhorse.brake_deceleration_mps2,
            ),
            (1.8, 4.2, 12.0, 13.5, 5.0, 5.0)
        );
        assert_eq!(
            (
                warhorse.turn_walk_deg_s,
                warhorse.turn_gallop_deg_s,
                warhorse.jump_apex_m,
                warhorse.jump_airtime_s,
                warhorse.max_vitality,
                warhorse.stagger_threshold,
                warhorse.sidestep_mps,
                warhorse.sidestep_ramp_s,
            ),
            (120.0, 45.0, 1.2, 0.5, 320.0, 90.0, 0.8, 0.35)
        );

        let mustang = HorseArchetype::Mustang.stats();
        assert_eq!(
            (
                mustang.walk_mps,
                mustang.trot_mps,
                mustang.gallop_mps,
                mustang.sprint_mps,
                mustang.accel_0_to_gallop_s,
                mustang.brake_deceleration_mps2,
            ),
            (1.9, 4.6, 13.0, 14.5, 3.5, 5.5)
        );
        assert_eq!(
            (
                mustang.turn_walk_deg_s,
                mustang.turn_gallop_deg_s,
                mustang.jump_apex_m,
                mustang.jump_airtime_s,
                mustang.max_vitality,
                mustang.stagger_threshold,
                mustang.sidestep_mps,
                mustang.sidestep_ramp_s,
            ),
            (150.0, 80.0, 2.2, 0.8, 250.0, 60.0, 1.2, 0.15)
        );
    }

    #[test]
    fn sidegrades_have_distinct_wins_and_shared_invariants() {
        let courser = HorseArchetype::Courser.stats();
        let warhorse = HorseArchetype::Warhorse.stats();
        let mustang = HorseArchetype::Mustang.stats();

        assert!(courser.gallop_mps > mustang.gallop_mps);
        assert!(mustang.gallop_mps > warhorse.gallop_mps);
        assert!(warhorse.max_vitality > mustang.max_vitality);
        assert!(warhorse.stagger_threshold > courser.stagger_threshold);
        assert!(mustang.turn_gallop_deg_s > courser.turn_gallop_deg_s);
        assert!(mustang.jump_apex_m > courser.jump_apex_m);
        assert!(mustang.terrain_mud > warhorse.terrain_mud);
        assert!(warhorse.terrain_mud > courser.terrain_mud);

        for stats in [courser, warhorse, mustang] {
            assert_eq!(stats.reverse_mps, SHARED_REVERSE_MPS);
            assert_eq!(stats.saddle_height_m, SHARED_SADDLE_HEIGHT_M);
        }
    }
}
