extends Node3D
class_name CombatEffects

@export var tracer_lifetime := 0.08
@export var impact_lifetime := 0.35

# Called only with positions returned by authority-facing hit/miss resolution. No raycast lives here.
func show_tracer(origin: Vector3, endpoint: Vector3, color: Color = Color("ffb84d")) -> Node3D:
	var length := origin.distance_to(endpoint)
	var tracer := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = Vector3(0.025, 0.025, maxf(length, 0.01))
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	material.emission_enabled = true
	material.emission = color
	material.emission_energy_multiplier = 2.0
	mesh.material = material
	tracer.mesh = mesh
	add_child(tracer)
	tracer.global_position = origin.lerp(endpoint, 0.5)
	var tracer_direction := tracer.global_position.direction_to(endpoint)
	tracer.look_at(endpoint, _safe_up(tracer_direction))
	_fade_and_free(tracer, tracer_lifetime)
	return tracer

func show_impact(position: Vector3, normal: Vector3, headshot := false) -> Node3D:
	var impact := MeshInstance3D.new()
	var mesh := CylinderMesh.new()
	mesh.top_radius = 0.09
	mesh.bottom_radius = 0.09
	mesh.height = 0.012
	var material := StandardMaterial3D.new()
	material.albedo_color = Color("ffd166") if headshot else Color("fff4d6")
	mesh.material = material
	impact.mesh = mesh
	add_child(impact)
	var impact_normal := normal.normalized() if normal.length_squared() > 0.0001 else Vector3.UP
	impact.global_position = position + impact_normal * 0.012
	impact.global_basis = Basis.looking_at(impact_normal, _safe_up(impact_normal)).rotated(Vector3.RIGHT, PI * 0.5)
	_fade_and_free(impact, impact_lifetime)
	return impact

func _safe_up(direction: Vector3) -> Vector3:
	return Vector3.FORWARD if absf(direction.normalized().dot(Vector3.UP)) > 0.98 else Vector3.UP

func _fade_and_free(node: Node, duration: float) -> void:
	var tween := create_tween()
	tween.tween_interval(duration)
	tween.tween_callback(node.queue_free)
