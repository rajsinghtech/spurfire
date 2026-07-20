extends Control
class_name SpurfireCaptureGate

signal quit_requested

@export var camera_rig: Node
@export var horse: Node
@export var gameplay: Node
@export var combat_input: Node

var captured := false
var capture_count := 0

@onready var dim: ColorRect = $Dim
@onready var card: PanelContainer = $Card
@onready var pill: Label = $CapturePill
@onready var quit_button: Button = $Card/Margin/VBox/Quit

func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	if camera_rig and camera_rig.has_signal(&"capture_state_changed"):
		camera_rig.capture_state_changed.connect(_on_capture_state_changed)
	quit_button.pressed.connect(_on_quit_pressed)
	_on_capture_state_changed(false)

func _gui_input(event: InputEvent) -> void:
	if captured:
		return
	if event is InputEventMouseButton:
		var button := event as InputEventMouseButton
		if button.button_index == MOUSE_BUTTON_LEFT and button.pressed:
			if quit_button and quit_button.get_global_rect().has_point(button.position):
				return
			request_capture()
			accept_event()

func request_capture() -> void:
	if captured:
		return
	capture_count += 1
	if camera_rig and camera_rig.has_method("request_capture"):
		camera_rig.call("request_capture")
	else:
		Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
		_on_capture_state_changed(true)
	# Apply this after capture-state notification so its ordinary enable does not
	# clear suppression of the click that opened the gate.
	_set_gameplay_enabled(true, true)

func release_capture() -> void:
	if camera_rig and camera_rig.has_method("release_capture"):
		camera_rig.call("release_capture", "gate_release")
	else:
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	_on_capture_state_changed(false)

func _on_capture_state_changed(value: bool) -> void:
	captured = value
	dim.visible = not captured
	card.visible = not captured
	mouse_filter = Control.MOUSE_FILTER_IGNORE if captured else Control.MOUSE_FILTER_STOP
	pill.text = "●  MOUSE CAPTURED" if captured else "○  MOUSE RELEASED"
	pill.modulate = Color("3fb6c9") if captured else Color("c44536")
	_set_gameplay_enabled(captured, false)

func _set_gameplay_enabled(enabled: bool, suppress_button: bool) -> void:
	if horse and horse.has_method("set_presentation_input_enabled"):
		horse.call("set_presentation_input_enabled", enabled, suppress_button)
	if gameplay and gameplay.has_method("set_presentation_input_enabled"):
		gameplay.call("set_presentation_input_enabled", enabled, suppress_button)
	if combat_input and combat_input.has_method("set_presentation_input_enabled"):
		combat_input.call("set_presentation_input_enabled", enabled, suppress_button)

func _on_quit_pressed() -> void:
	quit_requested.emit()
