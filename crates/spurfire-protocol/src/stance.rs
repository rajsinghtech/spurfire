//! Forward-compatible rider stance carried by snapshots and combat rewind rows.

use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Stable rider stance IDs shared by protocol JSON and Godot.
///
/// Unknown numeric values are retained so an older peer can relay and render a
/// conservative upright pose without accidentally granting a future gameplay
/// capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RiderStance {
    /// A reserved or future stance ID. ID zero is reserved and is represented
    /// by this variant as well.
    Unknown(u8),
    /// Rider is attached to a grounded horse.
    Mounted,
    /// Rider is attached to a horse that is jumping or falling.
    MountedAirborne,
    /// Rider is in the M2 flying-dismount window.
    SaddleDiveAirborne,
    /// Rider is in the no-input landing phase.
    LandingProne,
    /// Rider is in the half-speed landing recovery phase.
    LandingRecovery,
    /// Rider is standing on foot. M3 owns on-foot combat capability.
    OnFootStanding,
}

impl RiderStance {
    /// Reserved wire ID.
    pub const UNKNOWN_ID: u8 = 0;
    /// Grounded mounted wire ID.
    pub const MOUNTED_ID: u8 = 1;
    /// Ordinary mounted-airborne wire ID.
    pub const MOUNTED_AIRBORNE_ID: u8 = 2;
    /// Saddle Dive airborne wire ID.
    pub const SADDLE_DIVE_AIRBORNE_ID: u8 = 3;
    /// Landing-prone wire ID.
    pub const LANDING_PRONE_ID: u8 = 4;
    /// Landing-recovery wire ID.
    pub const LANDING_RECOVERY_ID: u8 = 5;
    /// On-foot standing wire ID.
    pub const ON_FOOT_STANDING_ID: u8 = 6;

    /// Decodes a numeric ID while preserving every future value.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            Self::MOUNTED_ID => Self::Mounted,
            Self::MOUNTED_AIRBORNE_ID => Self::MountedAirborne,
            Self::SADDLE_DIVE_AIRBORNE_ID => Self::SaddleDiveAirborne,
            Self::LANDING_PRONE_ID => Self::LandingProne,
            Self::LANDING_RECOVERY_ID => Self::LandingRecovery,
            Self::ON_FOOT_STANDING_ID => Self::OnFootStanding,
            other => Self::Unknown(other),
        }
    }

    /// Returns the unchanged protocol/Godot numeric ID.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Unknown(value) => value,
            Self::Mounted => Self::MOUNTED_ID,
            Self::MountedAirborne => Self::MOUNTED_AIRBORNE_ID,
            Self::SaddleDiveAirborne => Self::SADDLE_DIVE_AIRBORNE_ID,
            Self::LandingProne => Self::LANDING_PRONE_ID,
            Self::LandingRecovery => Self::LANDING_RECOVERY_ID,
            Self::OnFootStanding => Self::ON_FOOT_STANDING_ID,
        }
    }

    /// Whether this build knows the stance semantics.
    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// Whether the logical rider remains attached to a horse.
    #[must_use]
    pub const fn is_mounted(self) -> bool {
        matches!(self, Self::Mounted | Self::MountedAirborne)
    }

    /// Whether ordinary mounted weapon handling may remain unholstered.
    #[must_use]
    pub const fn carries_mounted_weapon(self) -> bool {
        matches!(
            self,
            Self::Mounted | Self::MountedAirborne | Self::SaddleDiveAirborne
        )
    }

    /// Stable telemetry spelling. Unknown values deliberately collapse to one
    /// conservative label while their numeric code remains available via
    /// [`Self::as_u8`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown(_) => "unknown",
            Self::Mounted => "mounted",
            Self::MountedAirborne => "mounted_airborne",
            Self::SaddleDiveAirborne => "saddle_dive_airborne",
            Self::LandingProne => "landing_prone",
            Self::LandingRecovery => "landing_recovery",
            Self::OnFootStanding => "on_foot_standing",
        }
    }
}

impl Default for RiderStance {
    /// Pre-M2 snapshots all represented a mounted rider, so defaulting to
    /// `Mounted` is the only backward-compatible missing-field behavior.
    fn default() -> Self {
        Self::Mounted
    }
}

impl Serialize for RiderStance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.as_u8())
    }
}

impl<'de> Deserialize<'de> for RiderStance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StanceVisitor;

        impl de::Visitor<'_> for StanceVisitor {
            type Value = RiderStance;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("one rider stance integer in 0..=255")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u8::try_from(value)
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))?;
                Ok(RiderStance::from_u8(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u8::try_from(value)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))?;
                Ok(RiderStance::from_u8(value))
            }
        }

        deserializer.deserialize_u8(StanceVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_ids_round_trip_as_one_json_number() {
        for value in 0_u8..=u8::MAX {
            let stance = RiderStance::from_u8(value);
            assert_eq!(stance.as_u8(), value);
            let encoded = serde_json::to_string(&stance).unwrap();
            assert_eq!(encoded, value.to_string());
            assert_eq!(
                serde_json::from_str::<RiderStance>(&encoded).unwrap(),
                stance
            );
        }
        assert_eq!(RiderStance::from_u8(0), RiderStance::Unknown(0));
        assert_eq!(RiderStance::from_u8(200), RiderStance::Unknown(200));
        assert!(!RiderStance::Unknown(200).is_known());
    }

    #[test]
    fn malformed_json_shapes_are_rejected() {
        for malformed in ["-1", "256", "1.5", "\"3\"", "null", "true", "{}"] {
            assert!(
                serde_json::from_str::<RiderStance>(malformed).is_err(),
                "accepted {malformed}"
            );
        }
    }

    #[test]
    fn default_is_the_legacy_mounted_pose() {
        assert_eq!(RiderStance::default(), RiderStance::Mounted);
        assert!(RiderStance::Mounted.is_mounted());
        assert!(RiderStance::MountedAirborne.is_mounted());
        assert!(!RiderStance::SaddleDiveAirborne.is_mounted());
        assert!(!RiderStance::Unknown(99).carries_mounted_weapon());
    }
}
