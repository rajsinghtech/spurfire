extends Node3D
class_name RiderPose

@export var rider: Node3D
@export var maximum_yaw_degrees_per_second := 360.0
@export var pose_response := 10.0
@export var remount_blend_seconds := 0.12

var _stance_id := 1
var _last_global := Transform3D.IDENTITY
var _remount_from := Transform3D.IDENTITY
var _remount_elapsed := -1.0

func _ready() -> void:
	if rider == null:
		rider = get_parent_node_3d()
	_last_global = global_transform
	if rider and rider.has_signal(&"stance_changed"):
		rider.stance_changed.connect(_on_stance_changed)

func _process(delta: float) -> void:
	if rider == null or not is_finite(delta) or delta <= 0.0:
		return
	_stance_id = int(rider.get("stance_id")) if rider.get("stance_id") != null else 0
	if _remount_elapsed >= 0.0:
		_remount_elapsed += delta
		var alpha := clampf(_remount_elapsed / maxf(remount_blend_seconds, 0.001), 0.0, 1.0)
		global_transform = _remount_from.interpolate_with(rider.global_transform, alpha)
		if alpha >= 1.0:
			top_level = false
			transform = Transform3D.IDENTITY
			_remount_elapsed = -1.0
		_last_global = global_transform
		return

	var velocity := Vector3.ZERO
	if rider is CharacterBody3D:
		velocity = (rider as CharacterBody3D).velocity
	elif rider.has_method("sample_at"):
		var sample := rider.call("sample_at", float(rider.get("render_tick"))) as Dictionary
		velocity = sample.get("velocity", Vector3.ZERO)
	var horizontal := Vector2(velocity.x, velocity.z).length()
	var target_pitch := 0.0
	match _stance_id:
		3:
			target_pitch = -atan2(velocity.y, maxf(horizontal, 0.001))
		4:
			target_pitch = deg_to_rad(78.0)
		5:
			target_pitch = deg_to_rad(28.0)
		_:
			target_pitch = 0.0
	var blend := 1.0 - exp(-maxf(pose_response, 0.01) * delta)
	rotation.x = lerp_angle(rotation.x, target_pitch, blend)
	rotation.z = lerp_angle(rotation.z, 0.0, blend)

	if _stance_id == 3 and horizontal > 0.01:
		var wanted_global_yaw := atan2(-velocity.x, -velocity.z)
		var wanted_local_yaw := wrapf(wanted_global_yaw - rider.global_rotation.y, -PI, PI)
		var max_step := deg_to_rad(maximum_yaw_degrees_per_second) * delta
		rotation.y = rotate_toward(rotation.y, wanted_local_yaw, max_step)
	else:
		rotation.y = rotate_toward(
			rotation.y,
			0.0,
			deg_to_rad(maximum_yaw_degrees_per_second) * delta
		)
	_last_global = global_transform

func _on_stance_changed(_previous_id: int, current_id: int, _tick: int, _dive_id: int) -> void:
	_stance_id = current_id
	if current_id == 1:
		_remount_from = _last_global
		_remount_elapsed = 0.0
		top_level = true
		global_transform = _remount_from
