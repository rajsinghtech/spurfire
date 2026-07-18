class_name SpurfireLobbyContract
extends RefCounted

const JOIN_CODE_PREFIX := "SPURFIRE1"
const SAFE_LOBBY_ERROR := "Lobby unavailable or invite code invalid. Check the code and try again."
const MIN_INVITATION_CHARS := 32
const MAX_INVITATION_CHARS := 512

static func service_origin_is_safe(origin: String) -> bool:
	var candidate := origin.strip_edges()
	if not candidate.begins_with("https://"):
		return false
	if candidate.contains("@") or candidate.contains("?") or candidate.contains("#"):
		return false
	var authority := candidate.trim_prefix("https://")
	return not authority.is_empty() and not authority.contains("/") and not authority.contains("\\")

static func clean_display_name(value: String) -> String:
	var cleaned := value.strip_edges()
	if cleaned.length() > 64:
		cleaned = cleaned.left(64)
	return cleaned

static func make_join_code(lobby_id: String, invitation: String) -> String:
	if not uuid_v4_is_valid(lobby_id) or not invitation_is_valid(invitation):
		return ""
	return "%s:%s:%s" % [JOIN_CODE_PREFIX, lobby_id.to_lower(), invitation]

static func parse_join_code(value: String) -> Dictionary:
	var parts := value.strip_edges().split(":", false, 3)
	if parts.size() != 3 or parts[0] != JOIN_CODE_PREFIX:
		return {}
	var lobby_id := str(parts[1]).to_lower()
	var invitation := str(parts[2])
	if not uuid_v4_is_valid(lobby_id) or not invitation_is_valid(invitation):
		return {}
	return {"lobby_id": lobby_id, "invitation": invitation}

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

static func new_idempotency_key() -> String:
	var random_bytes := Crypto.new().generate_random_bytes(24)
	var encoded := random_bytes.hex_encode()
	random_bytes.fill(0)
	return encoded

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

static func invitation_is_valid(value: String) -> bool:
	if value.length() < MIN_INVITATION_CHARS or value.length() > MAX_INVITATION_CHARS:
		return false
	for index in value.length():
		var scalar := value.unicode_at(index)
		var alphanumeric := (scalar >= 48 and scalar <= 57) or (scalar >= 65 and scalar <= 90) or (scalar >= 97 and scalar <= 122)
		if not alphanumeric and scalar != 45 and scalar != 95:
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
