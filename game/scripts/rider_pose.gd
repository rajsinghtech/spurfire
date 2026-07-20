extends Node3D
class_name RiderPose

@export var rider: Node3D
@export var maximum_yaw_degrees_per_second := 360.0
@export var pose_response := 10.0
@export var remount_blend_seconds := 0.12
@export var teleport_guard_metres := 8.0
@export var teleport_guard_degrees := 120.0

var _stance_id := 1
var _last_global := Transform3D.IDENTITY
var _last_rider_raw := Transform3D.IDENTITY
var _remount_from := Transform3D.IDENTITY
var _remount_elapsed := -1.0
var _visual_velocity := Vector3.ZERO
var _has_rider_raw := false
var _yaw_tick := -1
var _yaw_step_spent := 0.0
var _cape: MeshInstance3D
var _hat: Node3D

func _ready() -> void:
	if rider == null:
		rider = get_parent_node_3d()
	_last_global = global_transform
	if rider:
		_last_rider_raw = rider.global_transform
		_has_rider_raw = true
	if rider and rider.has_signal(&"stance_changed"):
		rider.stance_changed.connect(_on_stance_changed)
	_build_frontier_silhouette()

func _process(delta: float) -> void:
	if rider == null or not is_finite(delta) or delta <= 0.0:
		return
	var raw_rider := rider.global_transform
	if not _transform_is_finite(raw_rider):
		return
	if _is_teleport(raw_rider):
		rider.reset_physics_interpolation()
		reset_pose_interpolation()
	var rider_sample := rider.get_global_transform_interpolated()
	if not _transform_is_finite(rider_sample):
		return
	_last_rider_raw = raw_rider
	_has_rider_raw = true
	_stance_id = int(rider.get("stance_id")) if rider.get("stance_id") != null else 0

	if _remount_elapsed >= 0.0:
		_remount_elapsed += delta
		var alpha := clampf(_remount_elapsed / maxf(remount_blend_seconds, 0.001), 0.0, 1.0)
		global_transform = _remount_from.interpolate_with(rider_sample, alpha)
		rotation.x += sin(alpha * PI) * deg_to_rad(-22.0)
		if alpha >= 1.0:
			top_level = false
			transform = Transform3D.IDENTITY
			_remount_elapsed = -1.0
			_reset_yaw_limiter()
			reset_physics_interpolation()
		_last_global = global_transform
		return

	var velocity := Vector3.ZERO
	if rider is CharacterBody3D:
		velocity = (rider as CharacterBody3D).velocity
	elif rider.has_method("sample_at"):
		var sample := rider.call("sample_at", float(rider.get("render_tick"))) as Dictionary
		velocity = sample.get("velocity", Vector3.ZERO)
	var blend := 1.0 - exp(-maxf(pose_response, 0.01) * delta)
	_visual_velocity = _visual_velocity.lerp(velocity, blend)
	var horizontal := Vector2(_visual_velocity.x, _visual_velocity.z).length()
	var target_pitch := 0.0
	match _stance_id:
		3:
			target_pitch = -atan2(_visual_velocity.y, maxf(horizontal, 0.001))
		4:
			target_pitch = deg_to_rad(78.0)
		5:
			target_pitch = deg_to_rad(28.0)
		_:
			target_pitch = 0.0
	rotation.x = lerp_angle(rotation.x, target_pitch, blend)
	rotation.z = lerp_angle(rotation.z, 0.0, blend)
	if _cape:
		_cape.rotation.x = lerp_angle(_cape.rotation.x, deg_to_rad(18.0 + minf(horizontal, 14.0) * 2.2), blend)
		_cape.rotation.z = sin(Time.get_ticks_msec() * 0.012) * minf(horizontal / 14.0, 1.0) * 0.08
	if _hat:
		var hat_tilt := deg_to_rad(14.0) if _stance_id in [4, 5] else 0.0
		_hat.rotation.z = lerp_angle(_hat.rotation.z, hat_tilt, blend)

	if _stance_id == 3 and horizontal > 0.01:
		var wanted_global_yaw := atan2(-_visual_velocity.x, -_visual_velocity.z)
		var wanted_local_yaw := wrapf(
			wanted_global_yaw - rider_sample.basis.get_euler().y,
			-PI,
			PI
		)
		_apply_limited_yaw(wanted_local_yaw, delta)
	else:
		_apply_limited_yaw(0.0, delta)
	_last_global = global_transform

func _apply_limited_yaw(wanted_local_yaw: float, delta: float) -> void:
	var tick := Engine.get_physics_frames()
	if tick != _yaw_tick:
		_yaw_tick = tick
		_yaw_step_spent = 0.0
	var tick_budget := (
		deg_to_rad(maximum_yaw_degrees_per_second)
		/ maxf(float(Engine.physics_ticks_per_second), 1.0)
	)
	var applied := limited_yaw_change(
		rotation.y,
		wanted_local_yaw,
		delta,
		maximum_yaw_degrees_per_second,
		tick_budget - _yaw_step_spent
	)
	rotation.y = wrapf(rotation.y + applied, -PI, PI)
	_yaw_step_spent += absf(applied)

static func limited_yaw_change(
	current_yaw: float,
	wanted_yaw: float,
	delta: float,
	maximum_degrees_per_second: float,
	tick_budget_remaining: float
) -> float:
	var frame_budget := deg_to_rad(maxf(maximum_degrees_per_second, 0.0)) * maxf(delta, 0.0)
	var step := minf(frame_budget, maxf(tick_budget_remaining, 0.0))
	var shortest_arc := wrapf(wanted_yaw - current_yaw, -PI, PI)
	return clampf(shortest_arc, -step, step)

func _build_frontier_silhouette() -> void:
	var cream := StandardMaterial3D.new()
	cream.albedo_color = Color("f5e9d0")
	cream.roughness = 0.9
	_hat = Node3D.new()
	_hat.name = "FrontierHat"
	_hat.position = Vector3(0.0, 2.13, 0.0)
	var crown := MeshInstance3D.new()
	var crown_mesh := CylinderMesh.new()
	crown_mesh.top_radius = 0.19
	crown_mesh.bottom_radius = 0.23
	crown_mesh.height = 0.28
	crown_mesh.radial_segments = 12
	crown_mesh.material = cream
	crown.mesh = crown_mesh
	_hat.add_child(crown)
	var brim := MeshInstance3D.new()
	var brim_mesh := BoxMesh.new()
	brim_mesh.size = Vector3(0.72, 0.05, 0.62)
	brim_mesh.material = cream
	brim.mesh = brim_mesh
	brim.position.y = -0.14
	_hat.add_child(brim)
	add_child(_hat)
	_cape = MeshInstance3D.new()
	_cape.name = "SpeedCape"
	_cape.position = Vector3(0.0, 1.34, 0.28)
	var cape_mesh := BoxMesh.new()
	cape_mesh.size = Vector3(0.72, 0.72, 0.045)
	var cape_material := StandardMaterial3D.new()
	cape_material.albedo_color = Color("c44536")
	cape_material.roughness = 0.82
	cape_mesh.material = cape_material
	_cape.mesh = cape_mesh
	add_child(_cape)

func reset_pose_interpolation() -> void:
	_remount_elapsed = -1.0
	_reset_yaw_limiter()
	top_level = false
	transform = Transform3D.IDENTITY
	_last_global = global_transform
	if rider:
		_last_rider_raw = rider.global_transform
		_has_rider_raw = true
	reset_physics_interpolation()

func _reset_yaw_limiter() -> void:
	_yaw_tick = -1
	_yaw_step_spent = 0.0

func _is_teleport(raw_rider: Transform3D) -> bool:
	if not _has_rider_raw:
		return false
	var yaw_delta := absf(wrapf(
		raw_rider.basis.get_euler().y - _last_rider_raw.basis.get_euler().y,
		-PI,
		PI
	))
	return (
		raw_rider.origin.distance_to(_last_rider_raw.origin) > teleport_guard_metres
		or yaw_delta > deg_to_rad(teleport_guard_degrees)
	)

static func _transform_is_finite(value: Transform3D) -> bool:
	return value.origin.is_finite() and value.basis.is_finite()

func _on_stance_changed(_previous_id: int, current_id: int, _tick: int, _dive_id: int) -> void:
	_stance_id = current_id
	if current_id == 1:
		_remount_from = _last_global
		_remount_elapsed = 0.0
		_reset_yaw_limiter()
		top_level = true
		global_transform = _remount_from
		reset_physics_interpolation()
