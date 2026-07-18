class_name SpurfireLobbyHttpClient
extends Node

signal readiness_changed(create_authorized: bool, join_authorized: bool)
signal create_completed(response: Dictionary)
signal invitation_completed(join_code: String)
signal join_completed(response: Dictionary, enrollment_key: String)
signal lobby_updated(response: Dictionary)
signal network_updated(response: Dictionary)
signal endpoint_registered(response: Dictionary)
signal report_completed(response: Dictionary)
signal start_completed(response: Dictionary)
signal heartbeat_completed(response: Dictionary)
signal leave_completed(response: Dictionary)
signal end_completed(response: Dictionary)
signal request_failed(operation: String, safe_message: String)

const MAX_RESPONSE_BYTES := 64 * 1024
const REQUEST_TIMEOUT_SECONDS := 8.0
const WIRE_VERSION := "1.2"
const AUTHORITY_FORMULA := "election_v1"

var _origin := ""
var _player_id := ""
var _creator_capability := ""
var _participant_capability := ""
var _invitation_lobby_id := ""
var _requests: Array[HTTPRequest] = []
var _closed := false
var _last_endpoint_sequence := 0

func configure(service_origin: String, player_id: String) -> bool:
	if not SpurfireLobbyContract.service_origin_is_safe(service_origin):
		return false
	if not SpurfireLobbyContract.uuid_v4_is_valid(player_id):
		return false
	_origin = service_origin.strip_edges()
	_player_id = player_id
	return true

func has_creator_control() -> bool:
	return not _creator_capability.is_empty()

func has_participant_access() -> bool:
	return not _participant_capability.is_empty()

func probe_readiness() -> void:
	_request("readiness", HTTPClient.METHOD_GET, "/v1/capabilities", {}, "")

func create_lobby(display_name: String, one_use_create_grant: String) -> void:
	var cleaned := SpurfireLobbyContract.clean_display_name(display_name)
	if cleaned.is_empty() or one_use_create_grant.is_empty():
		request_failed.emit("create", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	# Public clients deliberately omit provisioning_mode. A ready server must
	# select tailnet_per_lobby; clients cannot opt into shared compatibility.
	_request(
		"create", HTTPClient.METHOD_POST, "/v1/lobbies",
		{"display_name": "%s's Posse" % cleaned, "max_players": 8},
		one_use_create_grant, true
	)
	one_use_create_grant = ""

func issue_invitation(lobby_id: String) -> void:
	if _creator_capability.is_empty():
		request_failed.emit("invitation", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	_invitation_lobby_id = lobby_id
	_request(
		"invitation", HTTPClient.METHOD_POST,
		"/v1/lobbies/%s/invitations" % lobby_id, {}, _creator_capability, true
	)

func join_lobby(lobby_id: String, invitation: String, display_name: String) -> void:
	var cleaned := SpurfireLobbyContract.clean_display_name(display_name)
	if not SpurfireLobbyContract.uuid_v4_is_valid(lobby_id) or cleaned.is_empty() or invitation.is_empty():
		request_failed.emit("join", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	_request(
		"join", HTTPClient.METHOD_POST, "/v1/lobbies/%s/join" % lobby_id,
		{
			"player_id": _player_id,
			"display_name": cleaned,
			"client_wire_version": WIRE_VERSION,
			"authority_formula_version": AUTHORITY_FORMULA,
			"horse_selection": "mustang",
		},
		invitation, true
	)
	invitation = ""

func poll_lobby(lobby_id: String) -> void:
	_request("lobby", HTTPClient.METHOD_GET, "/v1/lobbies/%s" % lobby_id, {}, _read_capability())

func poll_network(lobby_id: String) -> void:
	_request("network", HTTPClient.METHOD_GET, "/v1/lobbies/%s/network" % lobby_id, {}, _read_capability())

func register_endpoint(
	lobby_id: String, network_generation: int, roster_revision: int,
	address: String, port: int, session_public_key: String, key_proof: String,
	node_key: String = ""
) -> void:
	if (
		_participant_capability.is_empty() or network_generation <= 0
		or address.is_empty() or port <= 0 or port > 65535
		or session_public_key.is_empty() or key_proof.is_empty()
	):
		return
	var sequence := maxi(
		int(Time.get_unix_time_from_system() * 1000.0), _last_endpoint_sequence + 1
	)
	_last_endpoint_sequence = sequence
	var body := {
		"network_generation": network_generation,
		"roster_revision": roster_revision,
		# Wall-clock milliseconds survive client restart/re-key; process-relative
		# ticks would reset behind the server's cached value and permanently fail
		# its strictly-increasing replay gate while the server stays up.
		"sequence": sequence,
		"tailnet_address": address,
		"application_port": port,
		"session_public_key": session_public_key,
		"key_proof": key_proof,
	}
	if not node_key.is_empty():
		body["node_key"] = node_key
	_request(
		"endpoint", HTTPClient.METHOD_POST,
		"/v1/lobbies/%s/session/endpoint" % lobby_id,
		body, _participant_capability, true
	)

func submit_measurements(lobby_id: String, report: Dictionary) -> void:
	if _participant_capability.is_empty() or report.is_empty():
		return
	var body := report.duplicate(true)
	body["player_id"] = _player_id
	_request(
		"report", HTTPClient.METHOD_POST,
		"/v1/lobbies/%s/network/reports" % lobby_id,
		body, _participant_capability
	)
	body.clear()

func start_lobby(lobby_id: String) -> void:
	_request(
		"start", HTTPClient.METHOD_POST, "/v1/lobbies/%s/start" % lobby_id,
		{"creator_player_id": _player_id}, _creator_capability, true
	)

func authority_heartbeat(lobby_id: String, input_hash: String) -> void:
	if _participant_capability.is_empty() or input_hash.length() != 64:
		return
	_request(
		"heartbeat", HTTPClient.METHOD_POST,
		"/v1/lobbies/%s/heartbeat" % lobby_id,
		{"player_id": _player_id, "input_hash": input_hash},
		_participant_capability
	)

func leave_lobby(lobby_id: String) -> void:
	_request(
		"leave", HTTPClient.METHOD_POST, "/v1/lobbies/%s/leave" % lobby_id,
		{"player_id": _player_id}, _participant_capability, true
	)

func end_lobby(lobby_id: String) -> void:
	_request("end", HTTPClient.METHOD_DELETE, "/v1/lobbies/%s" % lobby_id, {}, _creator_capability, true)

func clear_lobby_secrets() -> void:
	_closed = true
	_creator_capability = ""
	_participant_capability = ""
	_invitation_lobby_id = ""
	for request in _requests:
		if is_instance_valid(request):
			request.cancel_request()
	_requests.clear()

func _exit_tree() -> void:
	clear_lobby_secrets()

func _read_capability() -> String:
	if not _participant_capability.is_empty():
		return _participant_capability
	return _creator_capability

func _request(operation: String, method: HTTPClient.Method, path: String, body: Dictionary, capability: String, idempotent: bool = false) -> void:
	if _closed or _origin.is_empty() or not path.begins_with("/"):
		request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	var request := HTTPRequest.new()
	request.timeout = REQUEST_TIMEOUT_SECONDS
	request.body_size_limit = MAX_RESPONSE_BYTES
	request.max_redirects = 0
	add_child(request)
	_requests.append(request)
	request.request_completed.connect(_on_request_completed.bind(operation, request), CONNECT_ONE_SHOT)
	var headers := PackedStringArray(["Accept: application/json", "Cache-Control: no-store"])
	var payload := ""
	if method in [HTTPClient.METHOD_POST, HTTPClient.METHOD_PUT, HTTPClient.METHOD_PATCH]:
		headers.append("Content-Type: application/json")
		payload = JSON.stringify(body)
	if not capability.is_empty():
		headers.append("Authorization: Spurfire-Capability %s" % capability)
	# The real-create grant authorizes the mutation; this UUID only binds the
	# resulting creator subject and is never treated as authentication.
	if operation == "create":
		headers.append("X-Spurfire-Player-Id: %s" % _player_id)
	if idempotent:
		headers.append("Idempotency-Key: %s" % SpurfireLobbyContract.new_idempotency_key())
	var result := request.request(_origin + path, headers, method, payload)
	capability = ""
	payload = ""
	if result != OK:
		_requests.erase(request)
		request.queue_free()
		request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)

func _on_request_completed(result: int, response_code: int, response_headers: PackedStringArray, body: PackedByteArray, operation: String, request: HTTPRequest) -> void:
	_requests.erase(request)
	request.queue_free()
	if _closed:
		body.fill(0)
		return
	if result != HTTPRequest.RESULT_SUCCESS or response_code < 200 or response_code >= 300 or body.size() > MAX_RESPONSE_BYTES or not _is_json_response(response_headers):
		body.fill(0)
		request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	var parsed = JSON.parse_string(body.get_string_from_utf8())
	body.fill(0)
	if not parsed is Dictionary:
		request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	var response := parsed as Dictionary
	match operation:
		"readiness":
			# Capability probes are not activation authorization. Missing explicit
			# product readiness stays closed on every old/current safe server.
			readiness_changed.emit(
				bool(response.get("real_lobby_creation_authorized", false)),
				bool(response.get("real_lobby_join_authorized", false))
			)
		"create":
			_creator_capability = _take_secret(response, "creator_capability")
			if _creator_capability.is_empty():
				request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
			else:
				create_completed.emit(_without_secrets(response))
		"invitation":
			var invitation := _take_secret(response, "invitation_code")
			if invitation.is_empty():
				invitation = _take_secret(response, "invitation_capability")
			if invitation.is_empty():
				invitation = _take_secret(response, "invitation")
			var lobby_id := str(response.get("lobby_id", _invitation_lobby_id))
			_invitation_lobby_id = ""
			var join_code := SpurfireLobbyContract.make_join_code(lobby_id, invitation)
			invitation = ""
			if join_code.is_empty():
				request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
			else:
				invitation_completed.emit(join_code)
		"join":
			_participant_capability = _take_secret(response, "participant_capability")
			var credential := response.get("join_credential", {}) as Dictionary
			var enrollment_key := _take_secret(credential, "auth_key")
			if _participant_capability.is_empty() or enrollment_key.is_empty() or enrollment_key == "DRY_RUN_NO_KEY":
				enrollment_key = ""
				request_failed.emit(operation, SpurfireLobbyContract.SAFE_LOBBY_ERROR)
			else:
				join_completed.emit(_without_secrets(response), enrollment_key)
				enrollment_key = ""
		"lobby":
			lobby_updated.emit(_without_secrets(response))
		"network":
			network_updated.emit(_without_secrets(response))
		"endpoint":
			endpoint_registered.emit(_without_secrets(response))
		"report":
			report_completed.emit(_without_secrets(response))
		"start":
			start_completed.emit(_without_secrets(response))
		"heartbeat":
			heartbeat_completed.emit(_without_secrets(response))
		"leave":
			leave_completed.emit(_without_secrets(response))
		"end":
			end_completed.emit(_without_secrets(response))

func _take_secret(container: Dictionary, key: String) -> String:
	var value = container.get(key, "")
	container.erase(key)
	if value is Dictionary:
		var nested := value as Dictionary
		var token := str(nested.get("token", nested.get("code", nested.get("capability", ""))))
		nested.clear()
		return token
	return str(value)

func _without_secrets(response: Dictionary) -> Dictionary:
	var safe := response.duplicate(true)
	for key in ["creator_capability", "participant_capability", "invitation_code", "invitation_capability", "invitation"]:
		safe.erase(key)
	var credential_value = safe.get("join_credential", null)
	if credential_value is Dictionary:
		(credential_value as Dictionary).erase("auth_key")
	# Session endpoints are consumed by lobby_peer_bridge and never rendered by
	# selected-lobby UI or included in support text.
	return safe

func _is_json_response(headers: PackedStringArray) -> bool:
	for header in headers:
		var normalized := str(header).to_lower()
		if normalized.begins_with("content-type:"):
			return normalized.contains("application/json")
	return false
