//! Snapshot interpolation and local-prediction reconciliation.

use std::collections::VecDeque;

pub const DEFAULT_INTERPOLATION_TICKS: u64 = 2;
pub const DEFAULT_MAX_EXTRAPOLATION_TICKS: u64 = 15;
pub const DEFAULT_SNAP_DISTANCE_M: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RiderState {
    pub tick: u64,
    pub position_m: [f32; 3],
    pub velocity_mps: [f32; 3],
    pub yaw_degrees: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampledRiderState {
    pub state: RiderState,
    pub extrapolated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reconciliation {
    pub position_error_m: [f32; 3],
    pub distance_m: f32,
    pub snap: bool,
}

#[derive(Clone, Debug)]
pub struct SnapshotBuffer {
    tick_rate: u32,
    capacity: usize,
    snapshots: VecDeque<RiderState>,
}

impl SnapshotBuffer {
    #[must_use]
    pub fn new(tick_rate: u32, capacity: usize) -> Self {
        Self {
            tick_rate: tick_rate.max(1),
            capacity: capacity.max(2),
            snapshots: VecDeque::with_capacity(capacity.max(2)),
        }
    }

    pub fn push(&mut self, snapshot: RiderState) -> bool {
        if !snapshot.position_m.iter().all(|value| value.is_finite())
            || !snapshot.velocity_mps.iter().all(|value| value.is_finite())
            || !snapshot.yaw_degrees.is_finite()
        {
            return false;
        }
        if self
            .snapshots
            .back()
            .is_some_and(|existing| snapshot.tick <= existing.tick)
        {
            return false;
        }
        self.snapshots.push_back(snapshot);
        while self.snapshots.len() > self.capacity {
            self.snapshots.pop_front();
        }
        true
    }

    #[must_use]
    pub fn latest_tick(&self) -> Option<u64> {
        self.snapshots.back().map(|snapshot| snapshot.tick)
    }

    #[must_use]
    pub fn sample(&self, render_tick: f64) -> Option<SampledRiderState> {
        if !render_tick.is_finite() || render_tick < 0.0 {
            return None;
        }
        let first = *self.snapshots.front()?;
        let latest = *self.snapshots.back()?;
        if render_tick <= first.tick as f64 {
            return Some(SampledRiderState {
                state: first,
                extrapolated: false,
            });
        }

        for (before, after) in self.snapshots.iter().zip(self.snapshots.iter().skip(1)) {
            if render_tick <= after.tick as f64 {
                let span = (after.tick - before.tick).max(1) as f32;
                let alpha = ((render_tick - before.tick as f64) as f32 / span).clamp(0.0, 1.0);
                return Some(SampledRiderState {
                    state: interpolate(*before, *after, alpha, render_tick.round() as u64),
                    extrapolated: false,
                });
            }
        }

        let requested_ticks = render_tick - latest.tick as f64;
        let extrapolation_ticks = requested_ticks.min(DEFAULT_MAX_EXTRAPOLATION_TICKS as f64);
        let seconds = extrapolation_ticks as f32 / self.tick_rate as f32;
        let mut state = latest;
        for axis in 0..3 {
            state.position_m[axis] += state.velocity_mps[axis] * seconds;
        }
        state.tick = latest
            .tick
            .saturating_add(extrapolation_ticks.round() as u64);
        Some(SampledRiderState {
            state,
            extrapolated: requested_ticks > 0.0,
        })
    }

    #[must_use]
    pub fn delayed_render_tick(&self) -> Option<f64> {
        self.latest_tick()
            .map(|tick| tick.saturating_sub(DEFAULT_INTERPOLATION_TICKS) as f64)
    }
}

#[must_use]
pub fn reconcile(predicted: RiderState, authoritative: RiderState) -> Reconciliation {
    let mut error = [0.0; 3];
    let mut squared = 0.0;
    for (axis, component) in error.iter_mut().enumerate() {
        *component = authoritative.position_m[axis] - predicted.position_m[axis];
        squared += *component * *component;
    }
    let distance = squared.sqrt();
    Reconciliation {
        position_error_m: error,
        distance_m: distance,
        snap: distance >= DEFAULT_SNAP_DISTANCE_M,
    }
}

fn interpolate(before: RiderState, after: RiderState, alpha: f32, tick: u64) -> RiderState {
    let mut position = [0.0; 3];
    let mut velocity = [0.0; 3];
    for axis in 0..3 {
        position[axis] =
            before.position_m[axis] + (after.position_m[axis] - before.position_m[axis]) * alpha;
        velocity[axis] = before.velocity_mps[axis]
            + (after.velocity_mps[axis] - before.velocity_mps[axis]) * alpha;
    }
    let yaw_delta = (after.yaw_degrees - before.yaw_degrees + 180.0).rem_euclid(360.0) - 180.0;
    RiderState {
        tick,
        position_m: position,
        velocity_mps: velocity,
        yaw_degrees: (before.yaw_degrees + yaw_delta * alpha).rem_euclid(360.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(tick: u64, x: f32, velocity: f32, yaw: f32) -> RiderState {
        RiderState {
            tick,
            position_m: [x, 0.0, 0.0],
            velocity_mps: [velocity, 0.0, 0.0],
            yaw_degrees: yaw,
        }
    }

    #[test]
    fn interpolation_handles_jitter_and_shortest_yaw_arc() {
        let mut buffer = SnapshotBuffer::new(60, 8);
        assert!(buffer.push(state(10, 0.0, 6.0, 350.0)));
        assert!(buffer.push(state(14, 4.0, 6.0, 10.0)));
        let sample = buffer.sample(12.0).unwrap();
        assert_eq!(sample.state.position_m, [2.0, 0.0, 0.0]);
        assert!(sample.state.yaw_degrees < 0.01 || sample.state.yaw_degrees > 359.99);
        assert!(!sample.extrapolated);
    }

    #[test]
    fn extrapolation_is_bounded_to_a_quarter_second() {
        let mut buffer = SnapshotBuffer::new(60, 8);
        assert!(buffer.push(state(20, 5.0, 12.0, 0.0)));
        let sample = buffer.sample(120.0).unwrap();
        assert!((sample.state.position_m[0] - 8.0).abs() < 0.001);
        assert_eq!(sample.state.tick, 35);
        assert!(sample.extrapolated);
    }

    #[test]
    fn stale_and_nonfinite_snapshots_are_rejected() {
        let mut buffer = SnapshotBuffer::new(60, 2);
        assert!(buffer.push(state(2, 0.0, 0.0, 0.0)));
        assert!(!buffer.push(state(2, 1.0, 0.0, 0.0)));
        assert!(!buffer.push(state(3, f32::NAN, 0.0, 0.0)));
    }

    #[test]
    fn reconciliation_distinguishes_smoothing_from_snap() {
        let predicted = state(10, 0.0, 0.0, 0.0);
        let close = reconcile(predicted, state(10, 0.25, 0.0, 0.0));
        assert!(!close.snap);
        assert!((close.distance_m - 0.25).abs() < f32::EPSILON);
        let far = reconcile(predicted, state(10, 3.0, 0.0, 0.0));
        assert!(far.snap);
    }
}
