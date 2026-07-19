class_name SpurfireLobbyContract
extends RefCounted

const SAFE_LOBBY_ERROR := "Lobby unavailable or invite code invalid. Check the code and try again."

static func clean_display_name(value: String) -> String:
	var cleaned := value.strip_edges()
	if cleaned.length() > 64:
		cleaned = cleaned.left(64)
	return cleaned

static func new_uuid_v4() -> String:
	var random_bytes := Crypto.new().generate_random_bytes(16)
	if random_bytes.size() != 16:
		return ""
	random_bytes[6] = (random_bytes[6] & 0x0f) | 0x40
	random_bytes[8] = (random_bytes[8] & 0x3f) | 0x80
	var encoded := random_bytes.hex_encode()
	random_bytes.fill(0)
	return "%s-%s-%s-%s-%s" % [
		encoded.substr(0, 8), encoded.substr(8, 4), encoded.substr(12, 4),
		encoded.substr(16, 4), encoded.substr(20, 12)
	]

static func uuid_v4_is_valid(value: String) -> bool:
	if value.length() != 36:
		return false
	for position in [8, 13, 18, 23]:
		if value[position] != "-":
			return false
	if value[14].to_lower() != "4" or value[19].to_lower() not in ["8", "9", "a", "b"]:
		return false
	for index in value.length():
		if index in [8, 13, 18, 23]:
			continue
		var scalar := value.unicode_at(index)
		if not ((scalar >= 48 and scalar <= 57) or (scalar >= 65 and scalar <= 70) or (scalar >= 97 and scalar <= 102)):
			return false
	return true

static func safe_roster(lobby: Dictionary, self_player_id: String, local_health: Dictionary) -> Array[Dictionary]:
	var authority_id := ""
	var authority_value = lobby.get("authority", {})
	if authority_value is Dictionary:
		authority_id = str((authority_value as Dictionary).get("candidate_player_id", ""))
	var rows: Array[Dictionary] = []
	var roster_value = lobby.get("roster", [])
	if not roster_value is Array:
		return rows
	for item in roster_value:
		if not item is Dictionary:
			continue
		var player := item as Dictionary
		var player_id := str(player.get("player_id", ""))
		var health := local_health.get(player_id, {}) as Dictionary
		rows.append({
			"player_id": player_id,
			"display_name": str(player.get("display_name", "Rider")),
			"you": player_id == self_player_id,
			"authority": player_id == authority_id,
			"route": str(health.get("route", "unknown")),
			"rtt_ms": health.get("rtt_ms", null),
			"freshness": str(health.get("freshness", "unknown")),
		})
	return rows

static func cleanup_message(network_view: Dictionary) -> String:
	var lifecycle := str((network_view.get("backing", {}) as Dictionary).get("network_lifecycle", ""))
	match lifecycle:
		"DEDICATED_ABSENT", "SHARED_RESOURCES_CLEAN":
			return "Confirmed closed — no lobby network remains."
		"VERIFYING_ABSENCE", "CLEANUP_PENDING":
			return "Still cleaning up — you can close the game; we'll keep at it."
		"MANUAL_REMEDIATION":
			return "Cleanup needs operator help. It is safe to close the game."
		_:
			return "Closing the private lobby…"
