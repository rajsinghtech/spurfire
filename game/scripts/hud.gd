extends CanvasLayer

@export var horse: Node
@export var rider: Node3D
var _log: FileAccess
const GAITS := ["Idle", "Walk", "Trot", "Gallop"]

func _ready() -> void:
	if horse and horse.has_signal("telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)
	_log = FileAccess.open("user://m0_telemetry.csv", FileAccess.WRITE)
	if _log:
		_log.store_line("time_ms,speed_mps,speed_kmh,gait,slope_angle_deg,surface,x,y,z,is_airborne")

func _process(_delta: float) -> void:
	var hint := $Panel/Margin/VBox/RemountHint as Label
	var retrievable := horse != null and bool(horse.get("is_retrievable"))
	var on_foot := rider != null and int(rider.get("stance_id")) == 6
	if not retrievable or not on_foot:
		hint.visible = false
		return
	var offset := rider.global_position - (horse as Node3D).global_position
	var distance := Vector2(offset.x, offset.z).length()
	hint.visible = true
	if distance <= 3.0:
		hint.text = "E — REMOUNT"
	else:
		hint.text = "HORSE READY  •  %.1f m" % distance

func _on_telemetry(data: Dictionary) -> void:
	var gait_index := clampi(int(data.get("gait", 0)), 0, GAITS.size() - 1)
	var speed := float(data.get("speed_mps", 0.0))
	var kmh := float(data.get("speed_kmh", speed * 3.6))
	var slope := float(data.get("slope_angle_deg", 0.0))
	$Panel/Margin/VBox/Readout.text = "%s  |  %.2f m/s  %.1f km/h  |  slope %.1f°" % [GAITS[gait_index], speed, kmh, slope]
	$Panel/Margin/VBox/Details.text = "surface: %s   airborne: %s   turn radius: %.1f m" % [data.get("surface", "flat"), data.get("is_airborne", false), float(data.get("turn_radius_m", 0.0))]
	$Panel/Margin/VBox/SpeedGraph.add_sample(speed)
	if _log:
		var position: Vector3 = data.get("position", Vector3.ZERO)
		_log.store_csv_line(PackedStringArray([str(Time.get_ticks_msec()), str(speed), str(kmh), str(gait_index), str(slope), str(data.get("surface", "flat")), str(position.x), str(position.y), str(position.z), str(data.get("is_airborne", false))]))
		_log.flush()
