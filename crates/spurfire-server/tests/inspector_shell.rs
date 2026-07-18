//! Static safety contract for the exact bytes served at `/inspect`.

const HTML: &str = include_str!("../src/inspect.html");

#[test]
fn inspector_selects_one_exact_lobby_without_a_directory() {
    assert!(HTML.contains("id=\"lobby\""));
    assert!(HTML.contains("id=\"capability\""));
    assert!(HTML.contains("/v1/lobbies/${encodeURIComponent(lobby)}/network"));
    assert!(HTML.contains("'Authorization': `Spurfire-Capability ${selectedCapability}`"));
    assert!(!HTML.contains("/v1/operator/lobbies"));
    assert!(!HTML.contains("fetch('/v1/lobbies'"));
}

#[test]
fn capability_is_memory_only_and_forgotten_on_exit() {
    for required in [
        "input.value = ''",
        "selectedCapability = ''",
        "window.addEventListener('pagehide'",
        "cache: 'no-store'",
        "credentials: 'omit'",
        "redirect: 'error'",
        "referrerPolicy: 'no-referrer'",
    ] {
        assert!(
            HTML.contains(required),
            "missing browser safeguard: {required}"
        );
    }
    for forbidden in [
        "localStorage",
        "sessionStorage",
        "document.cookie",
        "URLSearchParams",
        "console.",
        "?capability=",
    ] {
        assert!(!HTML.contains(forbidden), "secret sink found: {forbidden}");
    }
}

#[test]
fn response_is_schema_and_lobby_bound_and_failure_retains_stale_snapshot() {
    assert!(HTML.contains("body.schema_version !== 1"));
    assert!(HTML.contains("body.lobby_id !== selectedLobby"));
    assert!(HTML.contains("Previous snapshot retained as stale."));
    assert!(!HTML.contains("event.preventDefault(); view.replaceChildren()"));
}

#[test]
fn untrusted_strings_only_reach_text_content() {
    assert!(HTML.contains("heading.textContent = title"));
    assert!(HTML.contains("primary.textContent = text(value)"));
    assert!(HTML.contains("meta.textContent = detail || ''"));
    for sink in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
    ] {
        assert!(!HTML.contains(sink), "unsafe DOM sink found: {sink}");
    }
}

#[test]
fn output_uses_precise_truth_and_provenance_terms() {
    for required in [
        "Tailnet DNS name / FQDN",
        "Network lifecycle",
        "Provider-enrolled devices",
        "Fresh reporters",
        "Directional observations",
        "Peer Relay routes",
        "DERP Relay routes",
        "Application RTT median (ms)",
        "Control election winner",
    ] {
        assert!(
            HTML.contains(required),
            "missing inspector term: {required}"
        );
    }
    for forbidden_field in [
        "provider_tailnet_id",
        "oauth_client_secret",
        "node_private_key",
        "provider_device_id",
        "private_application_endpoint",
    ] {
        assert!(!HTML.contains(forbidden_field));
    }
}
