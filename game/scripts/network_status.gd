extends CanvasLayer

@export var peer_session: Node
@onready var label: Label = $Panel/Margin/Label

func _ready() -> void:
	if peer_session == null:
		label.text = "NET  OFFLINE"
		return
	peer_session.connected.connect(_on_connected)
	peer_session.connection_failed.connect(_on_failed)
	peer_session.disconnected.connect(_on_disconnected)
	_refresh()

func _refresh() -> void:
	var state := str(peer_session.get("connection_state")).to_upper()
	var ip := str(peer_session.get("tailnet_ip"))
	var port := int(peer_session.get("local_port"))
	label.text = "NET  %s" % state
	if not ip.is_empty():
		label.text += "  %s:%d" % [ip, port]

func _on_connected(_ip: String, _port: int) -> void:
	_refresh()

func _on_failed(message: String) -> void:
	label.text = "NET  ERROR"
	label.tooltip_text = message

func _on_disconnected() -> void:
	_refresh()
