//! Interpolated remote-rider presentation and prediction reconciliation for Godot.

use godot::classes::{INode3D, Node3D};
use godot::prelude::*;
use spurfire_net::replication::{
    reconcile, RiderState, SnapshotBuffer, DEFAULT_INTERPOLATION_TICKS,
    DEFAULT_MAX_EXTRAPOLATION_TICKS,
};
use spurfire_protocol::RiderStance;

#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct NetworkRider {
    #[base]
    base: Base<Node3D>,
    #[export]
    tick_rate: i64,
    #[export]
    auto_apply: bool,
    #[var(no_set)]
    latest_snapshot_tick: i64,
    #[var(no_set)]
    render_tick: f64,
    #[var(no_set)]
    extrapolating: bool,
    #[var(no_set)]
    stance_id: i64,
    #[var(no_set)]
    stance_known: bool,
    buffer: SnapshotBuffer,
    clock_started: bool,
}

#[godot_api]
impl NetworkRider {
    /// Insert a strictly newer authoritative snapshot. Every u8 stance is
    /// transport-valid; future values remain conservative and unknown.
    #[func]
    fn push_snapshot(
        &mut self,
        tick: i64,
        position: Vector3,
        velocity: Vector3,
        yaw_degrees: f64,
        stance_id: i64,
    ) -> bool {
        let (Ok(tick), Ok(stance_id)) = (u64::try_from(tick), u8::try_from(stance_id)) else {
            return false;
        };
        let stance = RiderStance::from_u8(stance_id);
        let snapshot = RiderState {
            tick,
            position_m: [position.x, position.y, position.z],
            velocity_mps: [velocity.x, velocity.y, velocity.z],
            yaw_degrees: yaw_degrees as f32,
            stance,
        };
        if !self.buffer.push(snapshot) {
            return false;
        }
        self.latest_snapshot_tick = i64::try_from(tick).unwrap_or(i64::MAX);
        let target_tick = tick.saturating_sub(DEFAULT_INTERPOLATION_TICKS) as f64;
        if !self.clock_started {
            self.render_tick = target_tick;
            self.clock_started = true;
            self.assign_stance(stance);
        } else {
            let drift = target_tick - self.render_tick;
            // Recover immediately after a control/path stall. Without this guard,
            // a free-running presentation clock can remain seconds ahead and make
            // fresh snapshots appear frozen while their ticks catch up.
            if drift.abs() > 12.0 {
                self.render_tick = target_tick;
            }
        }
        true
    }

    /// Sample without mutating the Node3D transform. Empty means no snapshots.
    #[func]
    fn sample_at(&self, render_tick: f64) -> VarDictionary {
        let Some(sample) = self.buffer.sample(render_tick) else {
            return VarDictionary::new();
        };
        sample_dictionary(sample.state, sample.extrapolated)
    }

    /// Compare local prediction to an authority snapshot. The currently
    /// presented stance is treated as prediction and the buffered state at the
    /// requested tick as authority, so stance correction is independent of the
    /// positional snap threshold.
    #[func]
    fn reconciliation(
        &self,
        tick: i64,
        predicted_position: Vector3,
        authoritative_position: Vector3,
    ) -> VarDictionary {
        let tick = tick.max(0).cast_unsigned();
        let predicted_stance = u8::try_from(self.stance_id)
            .map(RiderStance::from_u8)
            .unwrap_or(RiderStance::Unknown(0));
        let authoritative_stance = self
            .buffer
            .sample(tick as f64)
            .map_or(predicted_stance, |sample| sample.state.stance);
        let predicted = RiderState {
            tick,
            position_m: [
                predicted_position.x,
                predicted_position.y,
                predicted_position.z,
            ],
            velocity_mps: [0.0; 3],
            yaw_degrees: 0.0,
            stance: predicted_stance,
        };
        let authoritative = RiderState {
            tick,
            position_m: [
                authoritative_position.x,
                authoritative_position.y,
                authoritative_position.z,
            ],
            velocity_mps: [0.0; 3],
            yaw_degrees: 0.0,
            stance: authoritative_stance,
        };
        let correction = reconcile(predicted, authoritative);
        let mut result = VarDictionary::new();
        result.set(
            "position_error",
            Vector3::new(
                correction.position_error_m[0],
                correction.position_error_m[1],
                correction.position_error_m[2],
            ),
        );
        result.set("distance_m", f64::from(correction.distance_m));
        result.set("snap", correction.snap);
        result.set("stance_mismatch", correction.stance_mismatch);
        result.set("authoritative_stance_id", i64::from(authoritative_stance.as_u8()));
        result
    }
}

#[godot_api]
impl INode3D for NetworkRider {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            base,
            tick_rate: 60,
            auto_apply: true,
            latest_snapshot_tick: -1,
            render_tick: 0.0,
            extrapolating: false,
            stance_id: i64::from(RiderStance::MOUNTED_ID),
            stance_known: true,
            buffer: SnapshotBuffer::new(60, 32),
            clock_started: false,
        }
    }

    fn ready(&mut self) {
        self.tick_rate = self.tick_rate.clamp(1, 240);
        self.buffer = SnapshotBuffer::new(self.tick_rate as u32, 32);
    }

    fn process(&mut self, delta: f64) {
        if !self.clock_started || !delta.is_finite() || delta <= 0.0 {
            return;
        }
        let Some(latest_tick) = self.buffer.latest_tick() else {
            return;
        };
        let target_tick = latest_tick.saturating_sub(DEFAULT_INTERPOLATION_TICKS) as f64;
        let drift = target_tick - self.render_tick;
        let playback_rate = (1.0 + drift * 0.08).clamp(0.75, 1.25);
        self.render_tick += delta * self.tick_rate as f64 * playback_rate;
        self.render_tick = self
            .render_tick
            .min(latest_tick.saturating_add(DEFAULT_MAX_EXTRAPOLATION_TICKS) as f64);
        let Some(sample) = self.buffer.sample(self.render_tick) else {
            return;
        };
        self.extrapolating = sample.extrapolated;
        self.assign_stance(sample.state.stance);
        if self.auto_apply {
            self.base_mut().set_position(Vector3::new(
                sample.state.position_m[0],
                sample.state.position_m[1],
                sample.state.position_m[2],
            ));
            let mut rotation = self.base().get_rotation();
            rotation.y = sample.state.yaw_degrees.to_radians();
            self.base_mut().set_rotation(rotation);
        }
    }
}

impl NetworkRider {
    fn assign_stance(&mut self, stance: RiderStance) {
        self.stance_id = i64::from(stance.as_u8());
        self.stance_known = stance.is_known();
    }
}

fn sample_dictionary(state: RiderState, extrapolated: bool) -> VarDictionary {
    let mut result = VarDictionary::new();
    result.set("tick", i64::try_from(state.tick).unwrap_or(i64::MAX));
    result.set(
        "position",
        Vector3::new(
            state.position_m[0],
            state.position_m[1],
            state.position_m[2],
        ),
    );
    result.set(
        "velocity",
        Vector3::new(
            state.velocity_mps[0],
            state.velocity_mps[1],
            state.velocity_mps[2],
        ),
    );
    result.set("yaw_degrees", f64::from(state.yaw_degrees));
    result.set("stance_id", i64::from(state.stance.as_u8()));
    result.set("stance_known", state.stance.is_known());
    result.set("extrapolated", extrapolated);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stance_conversion_preserves_unknown_ids() {
        for id in 0_u8..=u8::MAX {
            let stance = RiderStance::from_u8(id);
            assert_eq!(stance.as_u8(), id);
        }
    }
}
