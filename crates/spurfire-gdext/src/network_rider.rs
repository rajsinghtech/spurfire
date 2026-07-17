//! Interpolated remote-rider presentation and prediction reconciliation for Godot.

use godot::classes::{INode3D, Node3D};
use godot::prelude::*;
use spurfire_net::replication::{reconcile, RiderState, SnapshotBuffer};

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
    buffer: SnapshotBuffer,
    clock_started: bool,
}

#[godot_api]
impl NetworkRider {
    /// Insert a strictly newer authoritative snapshot.
    #[func]
    fn push_snapshot(
        &mut self,
        tick: i64,
        position: Vector3,
        velocity: Vector3,
        yaw_degrees: f64,
    ) -> bool {
        let Ok(tick) = u64::try_from(tick) else {
            return false;
        };
        let snapshot = RiderState {
            tick,
            position_m: [position.x, position.y, position.z],
            velocity_mps: [velocity.x, velocity.y, velocity.z],
            yaw_degrees: yaw_degrees as f32,
        };
        if !self.buffer.push(snapshot) {
            return false;
        }
        self.latest_snapshot_tick = i64::try_from(tick).unwrap_or(i64::MAX);
        if !self.clock_started {
            self.render_tick = self.buffer.delayed_render_tick().unwrap_or(tick as f64);
            self.clock_started = true;
        }
        true
    }

    /// Sample without mutating the Node3D transform. Empty means no snapshots.
    #[func]
    fn sample_at(&self, render_tick: f64) -> VarDictionary {
        let Some(sample) = self.buffer.sample(render_tick) else {
            return VarDictionary::new();
        };
        let mut result = VarDictionary::new();
        result.set("tick", i64::try_from(sample.state.tick).unwrap_or(i64::MAX));
        result.set(
            "position",
            Vector3::new(
                sample.state.position_m[0],
                sample.state.position_m[1],
                sample.state.position_m[2],
            ),
        );
        result.set(
            "velocity",
            Vector3::new(
                sample.state.velocity_mps[0],
                sample.state.velocity_mps[1],
                sample.state.velocity_mps[2],
            ),
        );
        result.set("yaw_degrees", f64::from(sample.state.yaw_degrees));
        result.set("extrapolated", sample.extrapolated);
        result
    }

    /// Compare local prediction to an authority snapshot.
    #[func]
    fn reconciliation(
        &self,
        tick: i64,
        predicted_position: Vector3,
        authoritative_position: Vector3,
    ) -> VarDictionary {
        let tick = tick.max(0).cast_unsigned();
        let predicted = RiderState {
            tick,
            position_m: [
                predicted_position.x,
                predicted_position.y,
                predicted_position.z,
            ],
            velocity_mps: [0.0; 3],
            yaw_degrees: 0.0,
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
        self.render_tick += delta * self.tick_rate as f64;
        let Some(sample) = self.buffer.sample(self.render_tick) else {
            return;
        };
        self.extrapolating = sample.extrapolated;
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
