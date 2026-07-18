extends Node

const LOBBY_ID := "00000000-0000-4000-8000-000000000099"
const PLAYER_A := "00000000-0000-4000-8000-000000000001"
const PLAYER_B := "00000000-0000-4000-8000-000000000002"
const PLAYER_C := "00000000-0000-4000-8000-000000000003"
const INVITATION := "abcdefghijklmnopqrstuvwxyzABCDEFGH0123456789_-"

func _ready() -> void:
	var failures: Array[String] = []
	_check_origins(failures)
	_check_join_code(failures)
	_check_roster_projection(failures)
	_check_peer_roster_binding(failures)
	_check_secret_storage_contract(failures)
	_check_cleanup_truth(failures)
	if failures.is_empty():
		print("SPURFIRE_LOBBY_CLIENT_CONTRACT_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error(failure)
		get_tree().quit(1)

func _check_origins(failures: Array[String]) -> void:
	if not SpurfireLobbyContract.service_origin_is_safe("https://lobby.example.test"):
		failures.append("strict HTTPS origin was rejected")
	for bad in [
		"http://lobby.example.test", "https://user@lobby.example.test",
		"https://lobby.example.test/path", "https://lobby.example.test?token=x",
	]:
		if SpurfireLobbyContract.service_origin_is_safe(bad):
			failures.append("unsafe service origin accepted: %s" % bad)

func _check_join_code(failures: Array[String]) -> void:
	var code := SpurfireLobbyContract.make_join_code(LOBBY_ID, INVITATION)
	var decoded := SpurfireLobbyContract.parse_join_code(code)
	if str(decoded.get("lobby_id", "")) != LOBBY_ID or str(decoded.get("invitation", "")) != INVITATION:
		failures.append("one-use join code did not round trip exactly")
	for bad in ["", LOBBY_ID, "SPURFIRE1:%s:short" % LOBBY_ID, "SPURFIRE1:not-a-uuid:%s" % INVITATION]:
		if not SpurfireLobbyContract.parse_join_code(bad).is_empty():
			failures.append("malformed join code was accepted")

func _check_roster_projection(failures: Array[String]) -> void:
	var lobby := {
		"authority": {"candidate_player_id": PLAYER_A},
		"roster": [
			{"player_id": PLAYER_A, "display_name": "Dusty"},
			{"player_id": PLAYER_B, "display_name": "Mesa"},
		],
	}
	var rows := SpurfireLobbyContract.safe_roster(lobby, PLAYER_B, {
		PLAYER_A: {"route": "direct", "rtt_ms": 31, "freshness": "fresh"},
	})
	if rows.size() != 2 or not bool(rows[0].authority) or not bool(rows[1].you):
		failures.append("selected-lobby roster badges are incorrect")
	if rows[1].rtt_ms != null or str(rows[1].freshness) != "unknown":
		failures.append("missing health was fabricated instead of remaining unknown")
	for row: Dictionary in rows:
		if row.has("endpoint") or row.has("tailnet_address"):
			failures.append("player-visible roster exposed a private endpoint")

func _check_peer_roster_binding(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"PeerSession"):
		failures.append("PeerSession native class unavailable")
		return
	var receiver := ClassDB.instantiate(&"PeerSession") as Node
	var outsider := ClassDB.instantiate(&"PeerSession") as Node
	if receiver == null or outsider == null:
		failures.append("PeerSession could not be instantiated")
		return
	add_child(receiver)
	add_child(outsider)
	var roster := PackedStringArray([PLAYER_A, PLAYER_B])
	if not receiver.configure_roster_session(LOBBY_ID, PLAYER_A, PLAYER_A, roster, 1):
		failures.append("exact roster session configuration failed")
	if not outsider.configure_roster_session(
		LOBBY_ID, PLAYER_C, PLAYER_A, PackedStringArray([PLAYER_A, PLAYER_C]), 1
	):
		failures.append("outsider fixture session configuration failed")
	var outsider_packet: PackedByteArray = outsider.make_heartbeat(1)
	if receiver.accept_packet(outsider_packet, 2) != 4:
		failures.append("packet sender outside selected roster was not rejected")
	var leave_packet: PackedByteArray = receiver.make_leave(2)
	if str((receiver.decode_packet(leave_packet) as Dictionary).get("type", "")) != "leave":
		failures.append("orderly leave packet was not encoded")
	receiver.clear_lobby_session()
	outsider.clear_lobby_session()
	receiver.queue_free()
	outsider.queue_free()

func _check_secret_storage_contract(failures: Array[String]) -> void:
	var source := FileAccess.get_file_as_string("res://lobby/lobby_http_client.gd")
	for forbidden in ["FileAccess.open", "user://", "OS.set_environment", "print(", "push_error("]:
		if source.contains(forbidden):
			failures.append("lobby HTTP client contains forbidden secret sink: %s" % forbidden)
	if not source.contains("request.max_redirects = 0"):
		failures.append("lobby HTTP client does not disable redirects")
	if not source.contains("real_lobby_creation_authorized\", false"):
		failures.append("missing readiness must not default open")

func _check_cleanup_truth(failures: Array[String]) -> void:
	var pending := {"backing": {"network_lifecycle": "VERIFYING_ABSENCE"}}
	var absent := {"backing": {"network_lifecycle": "DEDICATED_ABSENT"}}
	if SpurfireLobbyContract.cleanup_message(pending).begins_with("Confirmed"):
		failures.append("delete acknowledgement was represented as confirmed cleanup")
	if not SpurfireLobbyContract.cleanup_message(absent).begins_with("Confirmed"):
		failures.append("exact absence did not produce confirmed cleanup copy")
