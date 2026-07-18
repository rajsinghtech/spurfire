//! Static safety and accessibility contract for the selected-lobby inspector shell.
//!
//! Routing and capability enforcement belong to the server integration branch. These tests keep
//! the non-authoritative browser shell from becoming a public directory or a secret sink.

const HTML: &str = include_str!("../src/landing.html");

#[test]
fn inspector_selects_exactly_one_lobby_without_a_directory() {
    assert!(HTML.contains("id=\"inspect-lobby-id\""));
    assert!(HTML.contains("id=\"inspect-capability\""));
    assert!(HTML.contains("A lobby ID by itself grants nothing."));
    assert!(HTML.contains("fetch(`/v1/lobbies/${encodeURIComponent(state.lobbyId)}/network`"));
    assert!(HTML.contains("'Authorization':`Spurfire-Capability ${state.capability}`"));

    assert!(!HTML.contains("/v1/operator/lobbies"));
    assert!(!HTML.contains("fetch('/v1/lobbies'"));
    assert!(!HTML.contains("lobby-search"));
    assert!(!HTML.contains("lobby-list"));
}

#[test]
fn capability_stays_out_of_urls_storage_and_dom_output() {
    assert!(HTML.contains("name=\"capability\" type=\"password\""));
    assert!(HTML.contains("capabilityInput.value=''"));
    assert!(HTML.contains("state.capability=null"));
    assert!(HTML.contains("window.addEventListener('pagehide'"));
    assert!(HTML.contains("cache:'no-store'"));
    assert!(HTML.contains("credentials:'omit'"));
    assert!(HTML.contains("redirect:'error'"));
    assert!(HTML.contains("referrerPolicy:'no-referrer'"));

    assert!(!HTML.contains("localStorage"));
    assert!(!HTML.contains("sessionStorage"));
    assert!(!HTML.contains("document.cookie"));
    assert!(!HTML.contains("?capability="));
    assert!(!HTML.contains("URLSearchParams"));
    assert!(!HTML.contains("console."));
}

#[test]
fn untrusted_response_values_only_reach_text_nodes() {
    assert!(HTML.contains("textContent=parts.value"));
    assert!(HTML.contains("textContent=parts.meta"));
    for forbidden_sink in [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
    ] {
        assert!(
            !HTML.contains(forbidden_sink),
            "unsafe DOM sink found: {forbidden_sink}"
        );
    }
    assert!(!HTML.contains("<script src="));
    assert!(!HTML.contains(" onload="));
    assert!(!HTML.contains(" onclick="));
    assert!(!HTML.contains(" onerror="));
}

#[test]
fn view_uses_precise_network_truth_and_freshness_terms() {
    for required in [
        "Tailnet DNS name / FQDN",
        "only <code>.net</code> is the TLD",
        "SIMULATED — NO TAILNET EXISTS",
        "REAL — DEDICATED TAILNET",
        "REAL — SHARED COMPATIBILITY",
        "Control service tailnet member",
        "lifecycle owner only; not joined",
        "Provider-enrolled devices",
        "Provider-online devices",
        "Fresh participant reporters",
        "Fresh directional observations",
        "Peer Relay",
        "DERP Relay",
        "Application RTT median",
        "Deterministic control election",
        "Last accepted heartbeat receipt",
        "Peer-reported match authority",
        "Exact absence confirmed",
        "Unknown —",
        "STALE ·",
    ] {
        assert!(
            HTML.contains(required),
            "missing inspector term: {required}"
        );
    }
    assert!(HTML.contains("does not join that network"));
    assert!(HTML.contains("not a tailnet member"));
    assert!(!HTML.contains("tailnet TLD"));
    assert!(!HTML.contains(".ts.net is the TLD"));
}

#[test]
fn member_projection_does_not_read_operator_or_secret_fields() {
    for forbidden_field in [
        "provider_tailnet_id",
        "secret_reference_present",
        "reconciliation_state",
        "lease_state",
        "last_poll_code",
        "self_tailnet_addresses",
        "provider_device_id",
        "physical_endpoint",
        "private_application_endpoint",
        "oauth_client_secret",
        "node_private_key",
    ] {
        assert!(
            !HTML.contains(forbidden_field),
            "forbidden response field found: {forbidden_field}"
        );
    }
    assert!(!HTML.contains("JSON.stringify(view)"));
    assert!(!HTML.contains("JSON.stringify(response)"));
}

#[test]
fn inspector_form_and_results_have_accessible_structure() {
    assert!(HTML.contains("<label for=\"inspect-lobby-id\">"));
    assert!(HTML.contains("<label for=\"inspect-capability\">"));
    assert!(HTML.contains("<fieldset class=\"inspect-form\">"));
    assert!(HTML.contains("role=\"status\" aria-live=\"polite\""));
    assert!(HTML.contains("aria-labelledby=\"network-result-title\" hidden"));
    assert!(HTML.contains("id=\"network-result-title\" tabindex=\"-1\""));
    assert!(HTML.contains("@media(prefers-reduced-motion:reduce)"));
    assert!(HTML.contains(":focus-visible"));
}
