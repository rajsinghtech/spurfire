extends CanvasLayer

@export var peer_session: Node
@export var replication: Node
@onready var label: Label = $Panel/Margin/Label
@onready var roster_panel: PanelContainer = $RosterPanel
@onready var roster_title: Label = $RosterPanel/Margin/VBox/Title
@onready var roster_label: Label = $RosterPanel/Margin/VBox/Rows
@onready var roster_hint: Label = $RosterPanel/Margin/VBox/Hint
@onready var match_panel: PanelContainer = $MatchPanel
@onready var match_clock: Label = $MatchPanel/Margin/Rows/MatchClock
@onready var match_pressure: Label = $MatchPanel/Margin/Rows/MatchPressure

var _refresh_accumulator := 0.0

func _ready() -> void:
	if peer_session == null:
		label.text = "NET  OFFLINE"
		return
	peer_session.connected.connect(_on_connected)
	peer_session.connection_failed.connect(_on_failed)
	peer_session.disconnected.connect(_on_disconnected)
	roster_panel.visible = false
	match_panel.visible = false
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
	_refresh_match_banner()

func _refresh_roster() -> void:
	if replication == null or not replication.has_method("get_peer_status"):
		roster_label.text = "No peer telemetry"
		return
	var match_state := _match_state()
	if not match_state.is_empty():
		_refresh_scoreboard(match_state)
		return
	roster_title.text = "RIDERS // LIVE ROUTES"
	roster_hint.text = "Private tailnet endpoints only • Hold TAB to inspect"
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
		var endpoint := str(peer.get("endpoint", "--"))
		lines.append("RIDER %s%-11s  %-10s  %7s  %s\n    ENDPOINT  %s" % [name, badges, route, rtt_text, health, endpoint])
	roster_label.text = "\n".join(lines)

func _refresh_match_banner() -> void:
	var state := _match_state()
	match_panel.visible = not state.is_empty()
	if state.is_empty():
		return
	var tick := int(state.get("current_tick", 0))
	var remaining_ticks := maxi(0, int(state.get("end_tick", 0)) - tick)
	var seconds := ceili(float(remaining_ticks) / 60.0)
	var local_score := 0
	var local_id := ""
	if replication != null:
		local_id = str(replication.get("local_player_id"))
	for row: Dictionary in state.get("players", []):
		if str(row.get("player_id", "")) == local_id:
			local_score = int(row.get("score", 0))
			break
	match_clock.text = "BOUNTY RUN   %02d:%02d   •   %d BOUNTY" % [seconds / 60, seconds % 60, local_score]
	var reveal := state.get("active_reveal", {}) as Dictionary
	var objective := state.get("active_objective", {}) as Dictionary
	if bool(state.get("finished", false)):
		var winner := str(state.get("winner", ""))
		match_pressure.text = "MATCH COMPLETE  •  %s WINS" % ("YOU" if winner == local_id else winner.left(8).to_upper())
		match_pressure.modulate = Color("ffd166")
	elif not reveal.is_empty():
		var wanted := str(reveal.get("player_id", ""))
		var wanted_seconds := ceili(float(maxi(0, int(reveal.get("end_tick", tick)) - tick)) / 60.0)
		match_pressure.text = "MOST WANTED: %s  •  REVEALED %ds" % ["YOU" if wanted == local_id else wanted.left(8).to_upper(), wanted_seconds]
		match_pressure.modulate = Color("ef4f58")
	elif not objective.is_empty():
		var objective_seconds := ceili(float(maxi(0, int(objective.get("end_tick", tick)) - tick)) / 60.0)
		match_pressure.text = "%s  •  %ds" % [_objective_name(str(objective.get("kind", ""))), objective_seconds]
		match_pressure.modulate = Color("8fe388")
	else:
		match_pressure.text = "RIDE • HUNT • CLAIM THE MOST BOUNTY"
		match_pressure.modulate = Color.WHITE

func _refresh_scoreboard(state: Dictionary) -> void:
	roster_title.text = "BOUNTY RUN // SCOREBOARD"
	roster_hint.text = "K kills • A assists • D deaths • route health remains visible"
	var peer_rows := {}
	for peer: Dictionary in replication.get_peer_status():
		peer_rows[str(peer.get("player_id", ""))] = peer
	var scores: Array = (state.get("players", []) as Array).duplicate(true)
	scores.sort_custom(func(left: Dictionary, right: Dictionary) -> bool:
		var left_score := int(left.get("score", 0))
		var right_score := int(right.get("score", 0))
		return (
			left_score > right_score
			or (left_score == right_score and str(left.get("player_id", "")) < str(right.get("player_id", "")))
		)
	)
	var tick := int(state.get("current_tick", 0))
	var wanted := str((state.get("active_reveal", {}) as Dictionary).get("player_id", ""))
	var lines: Array[String] = ["#   RIDER          BOUNTY    K   A   D   STATUS"]
	for index in range(scores.size()):
		var score := scores[index] as Dictionary
		var player_id := str(score.get("player_id", ""))
		var peer := peer_rows.get(player_id, {}) as Dictionary
		var display := "YOU" if bool(peer.get("you", false)) else player_id.left(8).to_upper()
		var badges := ""
		if bool(peer.get("authority", false)):
			badges += " HOST"
		if player_id == wanted:
			badges += " WANTED"
		var status := "RIDING"
		if not bool(score.get("alive", true)):
			var respawn_seconds := ceili(float(maxi(0, int(score.get("respawn_at_tick", tick)) - tick)) / 60.0)
			status = "RESPAWN %ds" % respawn_seconds
		elif int(score.get("speed_buff_end_tick", -1)) > tick:
			status = "SPEED +20%%"
		var route := _route_label(str(peer.get("route", "UNKNOWN")))
		var rtt := int(peer.get("rtt_ms", -1))
		lines.append("%-3d %-14s %6d  %3d %3d %3d   %s%s\n    %-10s  %s" % [
			index + 1, display, int(score.get("score", 0)),
			int(score.get("eliminations", 0)), int(score.get("assists", 0)),
			int(score.get("deaths", 0)), status, badges, route,
			"-- ms" if rtt < 0 else "%d ms" % rtt,
		])
	roster_label.text = "\n".join(lines)

func _match_state() -> Dictionary:
	if replication != null and replication.has_method("get_m5_state"):
		return replication.call("get_m5_state") as Dictionary
	return {}

func _objective_name(kind: String) -> String:
	return {
		"moving_bounty": "MOVING BOUNTY",
		"supply_herd": "SUPPLY HERD",
		"ammo_wagon": "AMMO WAGON",
		"signal_tower": "SIGNAL TOWER",
		"horse_buff_station": "HORSE BOND STATION",
	}.get(kind, "DYNAMIC OBJECTIVE")

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
