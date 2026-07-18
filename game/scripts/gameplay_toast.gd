extends Control
class_name GameplayToast

const ALLOWED_TEXT := [
	"FLYING DISMOUNT",
	"SADDLE DIVE HEADSHOT",
	"FULL-GALLOP HIT",
	"AIRBORNE REVERSAL",
]

@export var rider: Node
@export var display_seconds := 1.6
@onready var label: Label = %Notification

var _remaining := 0.0
var _queue: Array[String] = []

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	label.visible = false
	if rider and rider.has_signal(&"gameplay_event"):
		rider.gameplay_event.connect(_on_gameplay_event)

func _process(delta: float) -> void:
	if _remaining > 0.0:
		_remaining = maxf(0.0, _remaining - delta)
		if _remaining == 0.0:
			_show_next()

func _on_gameplay_event(_event_id: String, _kind: String, payload: Dictionary) -> void:
	var text := str(payload.get("text", ""))
	if text not in ALLOWED_TEXT:
		return
	_queue.append(text)
	if _remaining <= 0.0:
		_show_next()

func _show_next() -> void:
	if _queue.is_empty():
		label.visible = false
		return
	label.text = _queue.pop_front()
	label.visible = true
	# Burst events (for example a headshot reversal) remain attributable without
	# leaving the final notification several seconds behind the action.
	var backlog_scale := 1.0 + float(_queue.size())
	_remaining = maxf(display_seconds / backlog_scale, 0.35)
