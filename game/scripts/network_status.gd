extends CanvasLayer

@export var peer_session: Node
@export var replication: Node
@onready var label: Label = $Panel/Margin/Label
@onready var roster_panel: PanelContainer = $RosterPanel
@onready var roster_label: Label = $RosterPanel/Margin/VBox/Rows

var _refresh_accumulator := 0.0

func _ready() -> void:
	if peer_session == null:
		label.text = "NET  OFFLINE"
		return
	peer_session.connected.connect(_on_connected)
	peer_session.connection_failed.connect(_on_failed)
	peer_session.disconnected.connect(_on_disconnected)
	roster_panel.visible = false
	_refresh()

func _process(delta: float) -> void:
	roster_panel.visible = Input.is_action_pressed(&"scoreboard")
	_refresh_accumulator += delta
	if _refresh_accumulator < 0.2:
		return
	_refresh_accumulator = 0.0
	_refresh()
	if roster_panel.visible:
		_refresh_roster()

func _refresh() -> void:
	var state := str(peer_session.get("connection_state")).to_upper()
	var authority := str(peer_session.get("authority_player_id"))
	var epoch := int(peer_session.get("authority_epoch"))
	label.text = "NET  %s" % state
	if replication and replication.has_method("get_peer_status"):
		var peers := replication.get_peer_status() as Array
		if peers.size() > 1:
			var direct := 0
			for peer: Dictionary in peers:
				if str(peer.get("route", "")).contains("DIRECT"):
					direct += 1
			label.text += "  %d RIDERS  %d DIRECT" % [peers.size(), direct]
	if not authority.is_empty():
		label.text += "  HOST %s…  E%d" % [authority.left(8), epoch]
	label.text += "  [TAB]"

func _refresh_roster() -> void:
	if replication == null or not replication.has_method("get_peer_status"):
		roster_label.text = "No peer telemetry"
		return
	var lines: Array[String] = []
	for peer: Dictionary in replication.get_peer_status():
		var name := str(peer.get("name", "?")).to_upper()
		var badges := ""
		if bool(peer.get("you", false)):
			badges += "  YOU"
		if bool(peer.get("authority", false)):
			badges += "  HOST"
		var route := _route_label(str(peer.get("route", "UNKNOWN")))
		var rtt := int(peer.get("rtt_ms", -1))
		var rtt_text := "-- ms" if rtt < 0 else "%d ms" % rtt
		var age := int(peer.get("last_seen_ms", -1))
		var health := "WAITING" if age < 0 else ("LIVE" if age < 500 else "STALE")
		lines.append("RIDER %s%-11s  %-10s  %7s  %s" % [name, badges, route, rtt_text, health])
	roster_label.text = "\n".join(lines)

func _route_label(route: String) -> String:
	var upper := route.to_upper()
	if upper.contains("PEER") and upper.contains("RELAY"):
		return "PEER RELAY"
	if upper.contains("DERP"):
		return "DERP"
	if upper.contains("DIRECT"):
		return "DIRECT"
	if upper == "LOCAL":
		return "LOCAL"
	return "UNKNOWN"

func _on_connected(_ip: String, _port: int) -> void:
	_refresh()

func _on_failed(message: String) -> void:
	label.text = "NET  ERROR"
	label.tooltip_text = message

func _on_disconnected() -> void:
	_refresh()
