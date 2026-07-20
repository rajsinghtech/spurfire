extends Node3D
class_name SpurfireDustFx

const GAIT_CADENCE_HZ := [0.7, 1.9, 3.4, 5.2]
const CLEAN_RING := Color("f5e9d0")
const BAD_RING := Color("ff5c35")

@export var horse: Node3D
@export var rider: Node3D

var _speed_fraction := 0.0
var _lateral_fraction := 0.0
var _gait := 0
var _stride_phase := 0.0
var _was_airborne := false
var _skid_cooldown := 0.0

@onready var hoof_dust: GPUParticles3D = $HoofDust
@onready var landing_dust: GPUParticles3D = $LandingDust
@onready var skid_dust: GPUParticles3D = $SkidDust

func _ready() -> void:
	if horse and horse.has_signal(&"telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)
	if rider and rider.has_signal(&"dive_landed"):
		rider.dive_landed.connect(_on_dive_landed)

func _process(delta: float) -> void:
	if horse == null:
		return
	global_position = horse.global_position + Vector3(0, 0.08, 0.55)
	_skid_cooldown = maxf(0.0, _skid_cooldown - delta)
	var previous_phase := _stride_phase
	_stride_phase = fmod(_stride_phase + delta * GAIT_CADENCE_HZ[_gait], 1.0)
	if _gait >= 2 and _speed_fraction > 0.25 and _stride_phase < previous_phase:
		hoof_dust.restart()
		hoof_dust.emitting = true
	if absf(_lateral_fraction) > 0.55 and _speed_fraction > 0.4 and _skid_cooldown <= 0.0:
		skid_dust.position.x = -signf(_lateral_fraction) * 0.45
		skid_dust.restart()
		skid_dust.emitting = true
		_skid_cooldown = 0.18

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = clampf(float(data.get("speed_fraction", 0.0)), 0.0, 1.0)
	_lateral_fraction = clampf(float(data.get("lateral_speed_mps", 0.0)) / 1.2, -1.0, 1.0)
	_gait = clampi(int(data.get("gait", 0)), 0, GAIT_CADENCE_HZ.size() - 1)
	var airborne := bool(data.get("is_airborne", false))
	if _was_airborne and not airborne:
		_burst_landing(false, false)
	_was_airborne = airborne

func _on_dive_landed(_dive_id: int, bad: bool, _slope: float, _terrain: String) -> void:
	_burst_landing(bad, true)

func _burst_landing(bad: bool, show_ring: bool) -> void:
	landing_dust.global_position = rider.global_position if rider else global_position
	landing_dust.restart()
	landing_dust.emitting = true
	if show_ring:
		_spawn_landing_ring(bad)

func _spawn_landing_ring(bad: bool) -> void:
	var ring := MeshInstance3D.new()
	ring.name = "LandingRing"
	var mesh := TorusMesh.new()
	mesh.inner_radius = 0.42
	mesh.outer_radius = 0.5
	mesh.rings = 16
	mesh.ring_segments = 6
	var material := StandardMaterial3D.new()
	material.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_color = BAD_RING if bad else CLEAN_RING
	mesh.material = material
	ring.mesh = mesh
	ring.rotation.x = PI * 0.5
	ring.scale = Vector3.ONE * 0.3
	add_child(ring)
	ring.global_position = (rider.global_position if rider else global_position) + Vector3(0, 0.04, 0)
	var tween := create_tween()
	tween.set_parallel(true)
	tween.tween_property(ring, "scale", Vector3.ONE * 2.4, 0.35)
	tween.tween_property(material, "albedo_color:a", 0.0, 0.35)
	tween.chain().tween_callback(ring.queue_free)
