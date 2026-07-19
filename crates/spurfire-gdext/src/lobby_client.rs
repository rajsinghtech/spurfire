//! Native lobby control plane and human secret boundary.
//!
//! Bearer material is deliberately kept out of the Godot ABI. The only
//! Godot-visible values accepted or emitted here are public lobby/session data,
//! display names, IDs, booleans, and stable non-secret error codes.

use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::{
    io::Write,
    process::{Command, Stdio},
};

use godot::{
    classes::{Control, IControl, InputEvent, InputEventKey},
    prelude::*,
};
use reqwest::{
    header::{self, HeaderMap, HeaderValue},
    Method, Url,
};
use serde_json::{json, Value};
use spurfire_protocol::{LobbyId, PlayerId};
use zeroize::{Zeroize, Zeroizing};

pub(crate) const CONTROL_ORIGIN: &str = "https://spurfire.rajsingh.info";
const MAX_BODY: usize = 65_536;
const SAFE_ERROR: &str = "Lobby unavailable or invite code invalid. Check the code and try again.";
const JOIN_PREFIX: &[u8] = b"SPURFIRE1:";
const WIRE_VERSION: &str = "1.2";
const AUTHORITY_FORMULA: &str = "election_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LobbyOperation {
    Readiness,
    Create,
    CreatorJoin,
    Share,
    Join,
    Lobby,
    Network,
    Endpoint,
    Report,
    Start,
    Heartbeat,
    Leave,
    End,
}

impl LobbyOperation {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Readiness => "readiness",
            Self::Create => "create",
            Self::CreatorJoin => "join",
            Self::Share => "invitation",
            Self::Join => "join",
            Self::Lobby => "lobby",
            Self::Network => "network",
            Self::Endpoint => "endpoint",
            Self::Report => "report",
            Self::Start => "start",
            Self::Heartbeat => "heartbeat",
            Self::Leave => "leave",
            Self::End => "end",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Route<'a> {
    Readiness,
    Create,
    Lobby(&'a str),
    Network(&'a str),
    Invitation(&'a str),
    Join(&'a str),
    Endpoint(&'a str),
    Report(&'a str),
    Start(&'a str),
    Heartbeat(&'a str),
    Leave(&'a str),
    End(&'a str),
}

impl Route<'_> {
    fn path(self) -> Result<String, NativeLobbyError> {
        let lobby = |value: &str| {
            LobbyId::parse(value)
                .map(|id| id.to_string())
                .map_err(|_| NativeLobbyError::Route)
        };
        Ok(match self {
            Self::Readiness => "/v1/capabilities".into(),
            Self::Create => "/v1/lobbies".into(),
            Self::Lobby(id) => format!("/v1/lobbies/{}", lobby(id)?),
            Self::Network(id) => format!("/v1/lobbies/{}/network", lobby(id)?),
            Self::Invitation(id) => format!("/v1/lobbies/{}/invitations", lobby(id)?),
            Self::Join(id) => format!("/v1/lobbies/{}/join", lobby(id)?),
            Self::Endpoint(id) => format!("/v1/lobbies/{}/session/endpoint", lobby(id)?),
            Self::Report(id) => format!("/v1/lobbies/{}/network/reports", lobby(id)?),
            Self::Start(id) => format!("/v1/lobbies/{}/start", lobby(id)?),
            Self::Heartbeat(id) => format!("/v1/lobbies/{}/heartbeat", lobby(id)?),
            Self::Leave(id) => format!("/v1/lobbies/{}/leave", lobby(id)?),
            Self::End(id) => format!("/v1/lobbies/{}", lobby(id)?),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLobbyError {
    Cancelled,
    Client,
    Deadline,
    Route,
    Redirect,
    Status,
    Mime,
    Overflow,
    Json,
    Secret,
    Worker,
    Clipboard,
}

impl NativeLobbyError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Client => "transport",
            Self::Deadline => "deadline",
            Self::Route => "route",
            Self::Redirect => "redirect",
            Self::Status => "status",
            Self::Mime => "mime",
            Self::Overflow => "overflow",
            Self::Json => "json",
            Self::Secret => "secret",
            Self::Worker => "worker",
            Self::Clipboard => "clipboard",
        }
    }
}

impl fmt::Debug for NativeLobbyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

pub(crate) struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    fn new(bytes: Zeroizing<Vec<u8>>) -> Result<Self, NativeLobbyError> {
        if !(32..=512).contains(&bytes.len())
            || bytes.iter().any(|byte| {
                !byte.is_ascii_alphanumeric() && !matches!(*byte, b'-' | b'_' | b':' | b'.')
            })
        {
            return Err(NativeLobbyError::Secret);
        }
        Ok(Self(bytes))
    }

    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn into_zeroizing(self) -> Zeroizing<Vec<u8>> {
        self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(<redacted>)")
    }
}

pub(crate) struct NativeJoin {
    pub(crate) public_json: String,
    pub(crate) participant: SecretBytes,
    pub(crate) enrollment: SecretBytes,
}

pub(crate) enum LobbyEvent {
    Public {
        generation: u64,
        operation: LobbyOperation,
        json: String,
    },
    Created {
        generation: u64,
        public_json: String,
        creator: SecretBytes,
    },
    Invitation {
        generation: u64,
        creator_join: bool,
        lobby_id: String,
        invitation: SecretBytes,
    },
    Joined {
        generation: u64,
        joined: NativeJoin,
    },
    Failed {
        generation: u64,
        operation: LobbyOperation,
        error: NativeLobbyError,
    },
}

struct Cancellation {
    generation: AtomicU64,
    changed: tokio::sync::Notify,
}

impl Cancellation {
    fn new(generation: u64) -> Self {
        Self {
            generation: AtomicU64::new(generation),
            changed: tokio::sync::Notify::new(),
        }
    }

    fn cancel(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
        self.changed.notify_waiters();
    }

    async fn wait(&self, generation: u64) {
        loop {
            let changed = self.changed.notified();
            if self.generation.load(Ordering::Acquire) != generation {
                return;
            }
            changed.await;
        }
    }
}

pub(crate) struct LobbyClientState {
    generation: u64,
    player_id: Option<String>,
    creator: Option<SecretBytes>,
    participant: Option<SecretBytes>,
    copied_invitation: Option<Zeroizing<Vec<u8>>>,
    sender: Sender<LobbyEvent>,
    receiver: Receiver<LobbyEvent>,
    cancellation: Arc<Cancellation>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    pub(crate) last_endpoint_sequence: u64,
}

impl Default for LobbyClientState {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            generation: 1,
            player_id: None,
            creator: None,
            participant: None,
            copied_invitation: None,
            sender,
            receiver,
            cancellation: Arc::new(Cancellation::new(1)),
            workers: Arc::new(Mutex::new(Vec::new())),
            last_endpoint_sequence: 0,
        }
    }
}

impl LobbyClientState {
    pub(crate) fn configure_player(&mut self, player_id: &str) -> bool {
        if PlayerId::parse(player_id).is_err() {
            return false;
        }
        self.player_id = Some(player_id.to_owned());
        true
    }

    pub(crate) fn has_creator(&self) -> bool {
        self.creator.is_some()
    }

    pub(crate) fn has_participant(&self) -> bool {
        self.participant.is_some()
    }

    pub(crate) fn install_creator(&mut self, creator: SecretBytes) {
        self.creator = Some(creator);
    }

    pub(crate) fn install_participant(&mut self, participant: SecretBytes) {
        self.participant = Some(participant);
    }

    pub(crate) fn copy_invitation(
        &mut self,
        lobby_id: &str,
        invitation: &SecretBytes,
    ) -> Result<(), NativeLobbyError> {
        let code = assemble_invitation(lobby_id, invitation)?;
        if let Some(previous) = self.copied_invitation.take() {
            let _ = native_clipboard_clear_if_matches(&previous);
        }
        native_clipboard_write(&code)?;
        self.copied_invitation = Some(code);
        Ok(())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn player_id(&self) -> Option<&str> {
        self.player_id.as_deref()
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cancellation.cancel(self.generation);
        self.creator = None;
        self.participant = None;
        self.player_id = None;
        if let Some(value) = self.copied_invitation.take() {
            let _ = native_clipboard_clear_if_matches(&value);
        }
        while self.receiver.try_recv().is_ok() {}
        let handles = self
            .workers
            .lock()
            .map(|mut workers| workers.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in handles {
            let _ = handle.join();
        }
    }

    pub(crate) fn try_event(&self) -> Result<LobbyEvent, TryRecvError> {
        reap_finished_workers(&self.workers);
        self.receiver.try_recv()
    }

    fn capability(&self, creator: bool) -> Option<&SecretBytes> {
        if creator {
            self.creator.as_ref()
        } else {
            self.participant.as_ref().or(self.creator.as_ref())
        }
    }

    pub(crate) fn request_public(
        &self,
        operation: LobbyOperation,
        method: Method,
        route: Route<'_>,
        body: Option<String>,
        creator_capability: bool,
        idempotent: bool,
    ) {
        let capability = self.capability(creator_capability);
        self.spawn(operation, method, route, body, capability, idempotent, None);
    }

    pub(crate) fn create(&self, display_name: &str, grant: SecretBytes) {
        let Some(player_id) = &self.player_id else {
            self.fail_now(LobbyOperation::Create, NativeLobbyError::Route);
            return;
        };
        let body = json!({
            "display_name": format!("{display_name}'s Posse"),
            "max_players": 8
        })
        .to_string();
        self.spawn(
            LobbyOperation::Create,
            Method::POST,
            Route::Create,
            Some(body),
            Some(&grant),
            true,
            Some(player_id.clone()),
        );
    }

    pub(crate) fn invitation(&self, lobby_id: &str, creator_join: bool) {
        let operation = if creator_join {
            LobbyOperation::CreatorJoin
        } else {
            LobbyOperation::Share
        };
        let Ok(path) = Route::Invitation(lobby_id).path() else {
            self.fail_now(operation, NativeLobbyError::Route);
            return;
        };
        let Some(capability) = self.creator.as_ref() else {
            self.fail_now(operation, NativeLobbyError::Secret);
            return;
        };
        let sender = self.sender.clone();
        let generation = self.generation;
        let lobby_id = lobby_id.to_owned();
        let auth = sensitive_authorization(capability);
        spawn_request(
            move || {
                let result = async {
                    let auth = auth?;
                    // Once dispatched, a first-response-only invitation must reach native
                    // ownership before cancellation can complete.
                    let mut body = request_uncancelled(
                        Method::POST,
                        &path,
                        Some("{}".into()),
                        Some(auth),
                        true,
                        None,
                    )
                    .await?;
                    let invitation = extract_secret(&mut body, b"token")?;
                    let public_json = public_json(body)?;
                    let _ = public_json;
                    Ok::<_, NativeLobbyError>(invitation)
                };
                let event = match run_async(result) {
                    Ok(invitation) => LobbyEvent::Invitation {
                        generation,
                        creator_join,
                        lobby_id,
                        invitation,
                    },
                    Err(error) => LobbyEvent::Failed {
                        generation,
                        operation,
                        error,
                    },
                };
                let _ = sender.send(event);
            },
            self.sender.clone(),
            self.generation,
            operation,
            &self.workers,
        );
    }

    pub(crate) fn join(&self, lobby_id: &str, display_name: &str, invitation: SecretBytes) {
        let Some(player_id) = &self.player_id else {
            self.fail_now(LobbyOperation::Join, NativeLobbyError::Route);
            return;
        };
        let body = json!({
            "player_id": player_id,
            "display_name": display_name,
            "client_wire_version": WIRE_VERSION,
            "authority_formula_version": AUTHORITY_FORMULA,
            "horse_selection": "mustang"
        })
        .to_string();
        self.spawn_join(lobby_id, body, invitation);
    }

    pub(crate) fn join_creator(&self, lobby_id: &str, display_name: &str, invitation: SecretBytes) {
        self.join(lobby_id, display_name, invitation);
    }

    fn spawn_join(&self, lobby_id: &str, body: String, invitation: SecretBytes) {
        let Ok(path) = Route::Join(lobby_id).path() else {
            self.fail_now(LobbyOperation::Join, NativeLobbyError::Route);
            return;
        };
        let sender = self.sender.clone();
        let generation = self.generation;
        let auth = sensitive_authorization(&invitation);
        spawn_request(
            move || {
                let result = async {
                    let auth = auth?;
                    // Joining consumes the invitation. Do not race that mutation against
                    // cancellation and discard the only enrollment/capability response.
                    let mut response = request_uncancelled(
                        Method::POST,
                        &path,
                        Some(body),
                        Some(auth),
                        true,
                        None,
                    )
                    .await?;
                    let enrollment = extract_secret(&mut response, b"auth_key")?;
                    if enrollment.as_bytes() == b"DRY_RUN_NO_KEY" {
                        return Err(NativeLobbyError::Secret);
                    }
                    let participant = extract_secret(&mut response, b"token")?;
                    let public_json = public_json(response)?;
                    Ok::<_, NativeLobbyError>(NativeJoin {
                        public_json,
                        participant,
                        enrollment,
                    })
                };
                let event = match run_async(result) {
                    Ok(joined) => LobbyEvent::Joined { generation, joined },
                    Err(error) => LobbyEvent::Failed {
                        generation,
                        operation: LobbyOperation::Join,
                        error,
                    },
                };
                let _ = sender.send(event);
            },
            self.sender.clone(),
            self.generation,
            LobbyOperation::Join,
            &self.workers,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &self,
        operation: LobbyOperation,
        method: Method,
        route: Route<'_>,
        body: Option<String>,
        capability: Option<&SecretBytes>,
        idempotent: bool,
        actor: Option<String>,
    ) {
        let Ok(path) = route.path() else {
            self.fail_now(operation, NativeLobbyError::Route);
            return;
        };
        let auth = capability.map(sensitive_authorization).transpose();
        let sender = self.sender.clone();
        let generation = self.generation;
        let cancellation = self.cancellation.clone();
        spawn_request(
            move || {
                let result = async {
                    let mut response = if operation == LobbyOperation::Create {
                        // A real create consumes its grant and singleton lease. Finish the
                        // response handoff before honoring cancellation.
                        request_uncancelled(method, &path, body, auth?, idempotent, actor).await?
                    } else {
                        request(
                            method,
                            &path,
                            body,
                            auth?,
                            idempotent,
                            actor,
                            cancellation,
                            generation,
                        )
                        .await?
                    };
                    match operation {
                        LobbyOperation::Create => {
                            let creator = extract_secret(&mut response, b"token")?;
                            let public_json = public_json(response)?;
                            Ok(SpawnResult::Created(public_json, creator))
                        }
                        _ => Ok(SpawnResult::Public(public_json(response)?)),
                    }
                };
                let event = match run_async(result) {
                    Ok(SpawnResult::Created(public_json, creator)) => LobbyEvent::Created {
                        generation,
                        public_json,
                        creator,
                    },
                    Ok(SpawnResult::Public(json)) => LobbyEvent::Public {
                        generation,
                        operation,
                        json,
                    },
                    Err(error) => LobbyEvent::Failed {
                        generation,
                        operation,
                        error,
                    },
                };
                let _ = sender.send(event);
            },
            self.sender.clone(),
            self.generation,
            operation,
            &self.workers,
        );
    }

    pub(crate) fn fail_now(&self, operation: LobbyOperation, error: NativeLobbyError) {
        let _ = self.sender.send(LobbyEvent::Failed {
            generation: self.generation,
            operation,
            error,
        });
    }
}

enum SpawnResult {
    Public(String),
    Created(String, SecretBytes),
}

fn reap_finished_workers(workers: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    let finished = if let Ok(mut active) = workers.lock() {
        let mut finished = Vec::new();
        let mut index = 0;
        while index < active.len() {
            if active[index].is_finished() {
                finished.push(active.swap_remove(index));
            } else {
                index += 1;
            }
        }
        finished
    } else {
        Vec::new()
    };
    for handle in finished {
        let _ = handle.join();
    }
}

fn spawn_request(
    task: impl FnOnce() + Send + 'static,
    fallback: Sender<LobbyEvent>,
    generation: u64,
    operation: LobbyOperation,
    workers: &Arc<Mutex<Vec<JoinHandle<()>>>>,
) {
    reap_finished_workers(workers);
    match thread::Builder::new()
        .name(format!("spurfire-lobby-{}", operation.code()))
        .spawn(task)
    {
        Ok(handle) => {
            if let Ok(mut active) = workers.lock() {
                active.push(handle);
            }
        }
        Err(_) => {
            let _ = fallback.send(LobbyEvent::Failed {
                generation,
                operation,
                error: NativeLobbyError::Worker,
            });
        }
    }
}

fn run_async<T>(
    future: impl std::future::Future<Output = Result<T, NativeLobbyError>>,
) -> Result<T, NativeLobbyError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| NativeLobbyError::Client)?;
    runtime.block_on(future)
}

fn build_client() -> Result<reqwest::Client, NativeLobbyError> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| NativeLobbyError::Client)
}

fn exact_url(path: &str) -> Result<Url, NativeLobbyError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('?')
        || path.contains('#')
        || path.contains("..")
        || path.to_ascii_lowercase().contains("%2e")
        || path.contains('\\')
    {
        return Err(NativeLobbyError::Route);
    }
    let origin = Url::parse(CONTROL_ORIGIN).map_err(|_| NativeLobbyError::Route)?;
    let url = origin.join(path).map_err(|_| NativeLobbyError::Route)?;
    if url.scheme() != "https"
        || url.host_str() != origin.host_str()
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != format!("{CONTROL_ORIGIN}{path}")
    {
        return Err(NativeLobbyError::Route);
    }
    Ok(url)
}

fn sensitive_authorization(secret: &SecretBytes) -> Result<HeaderValue, NativeLobbyError> {
    let mut value = Zeroizing::new(Vec::with_capacity(20 + secret.as_bytes().len()));
    value.extend_from_slice(b"Spurfire-Capability ");
    value.extend_from_slice(secret.as_bytes());
    let mut header = HeaderValue::from_bytes(&value).map_err(|_| NativeLobbyError::Secret)?;
    header.set_sensitive(true);
    Ok(header)
}

fn idempotency_key() -> Result<HeaderValue, NativeLobbyError> {
    let mut random = Zeroizing::new([0_u8; 24]);
    getrandom::getrandom(&mut *random).map_err(|_| NativeLobbyError::Client)?;
    let encoded: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    HeaderValue::from_str(&encoded).map_err(|_| NativeLobbyError::Client)
}

#[allow(clippy::too_many_arguments)]
async fn request(
    method: Method,
    path: &str,
    body: Option<String>,
    authorization: Option<HeaderValue>,
    idempotent: bool,
    actor: Option<String>,
    cancellation: Arc<Cancellation>,
    generation: u64,
) -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    tokio::select! {
        result = request_uncancelled(method, path, body, authorization, idempotent, actor) => result,
        () = wait_for_cancellation(cancellation, generation) => Err(NativeLobbyError::Cancelled),
    }
}

async fn wait_for_cancellation(cancellation: Arc<Cancellation>, generation: u64) {
    cancellation.wait(generation).await;
}

async fn request_uncancelled(
    method: Method,
    path: &str,
    body: Option<String>,
    authorization: Option<HeaderValue>,
    idempotent: bool,
    actor: Option<String>,
) -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    let client = build_client()?;
    let url = exact_url(path)?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if body.is_some() {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    if let Some(value) = authorization {
        headers.insert(header::AUTHORIZATION, value);
    }
    if idempotent {
        headers.insert("idempotency-key", idempotency_key()?);
    }
    if let Some(actor) = actor {
        headers.insert(
            "x-spurfire-player-id",
            HeaderValue::from_str(&actor).map_err(|_| NativeLobbyError::Route)?,
        );
    }
    let mut builder = client.request(method, url).headers(headers);
    if let Some(body) = body {
        builder = builder.body(body);
    }
    let mut response = builder.send().await.map_err(|error| {
        if error.is_timeout() {
            NativeLobbyError::Deadline
        } else {
            NativeLobbyError::Client
        }
    })?;
    if response.status().is_redirection() {
        return Err(NativeLobbyError::Redirect);
    }
    if !response.status().is_success() {
        return Err(NativeLobbyError::Status);
    }
    strict_json_mime(response.headers())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(NativeLobbyError::Overflow);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BODY));
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            NativeLobbyError::Deadline
        } else {
            NativeLobbyError::Client
        }
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(NativeLobbyError::Overflow);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn strict_json_mime(headers: &HeaderMap) -> Result<(), NativeLobbyError> {
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(NativeLobbyError::Mime)?;
    let media = value.split(';').next().unwrap_or_default().trim();
    if media.eq_ignore_ascii_case("application/json") {
        Ok(())
    } else {
        Err(NativeLobbyError::Mime)
    }
}

fn extract_secret(
    body: &mut Zeroizing<Vec<u8>>,
    key: &[u8],
) -> Result<SecretBytes, NativeLobbyError> {
    let mut needle = Vec::with_capacity(key.len() + 2);
    needle.push(b'"');
    needle.extend_from_slice(key);
    needle.push(b'"');
    let matches = body
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, candidate)| (candidate == needle).then_some(index))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(NativeLobbyError::Secret);
    }
    let mut cursor = matches[0] + needle.len();
    while body.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if body.get(cursor) != Some(&b':') {
        return Err(NativeLobbyError::Json);
    }
    cursor += 1;
    while body.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if body.get(cursor) != Some(&b'"') {
        return Err(NativeLobbyError::Secret);
    }
    let start = cursor + 1;
    cursor = start;
    while let Some(byte) = body.get(cursor).copied() {
        if byte == b'"' {
            break;
        }
        if byte == b'\\' || !byte.is_ascii() || byte.is_ascii_control() {
            return Err(NativeLobbyError::Secret);
        }
        cursor += 1;
    }
    if body.get(cursor) != Some(&b'"') {
        return Err(NativeLobbyError::Json);
    }
    let secret = SecretBytes::new(Zeroizing::new(body[start..cursor].to_vec()))?;
    body[start..cursor].fill(b' ');
    Ok(secret)
}

fn public_json(mut body: Zeroizing<Vec<u8>>) -> Result<String, NativeLobbyError> {
    let mut value: Value = serde_json::from_slice(&body).map_err(|_| NativeLobbyError::Json)?;
    remove_secret_members(&mut value);
    body.zeroize();
    serde_json::to_string(&value).map_err(|_| NativeLobbyError::Json)
}

fn remove_secret_members(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in [
                "creator_capability",
                "participant_capability",
                "invitation",
                "invitation_code",
                "invitation_capability",
                "auth_key",
            ] {
                map.remove(key);
            }
            for value in map.values_mut() {
                remove_secret_members(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(remove_secret_members),
        _ => {}
    }
}

pub(crate) fn parse_join_code(
    mut bytes: Zeroizing<Vec<u8>>,
) -> Result<(String, SecretBytes), NativeLobbyError> {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes.remove(0);
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes.pop();
    }
    if !bytes.starts_with(JOIN_PREFIX) {
        return Err(NativeLobbyError::Secret);
    }
    let rest = &bytes[JOIN_PREFIX.len()..];
    let split = rest
        .iter()
        .position(|byte| *byte == b':')
        .ok_or(NativeLobbyError::Secret)?;
    let lobby = std::str::from_utf8(&rest[..split]).map_err(|_| NativeLobbyError::Secret)?;
    let lobby_id = LobbyId::parse(lobby)
        .map_err(|_| NativeLobbyError::Route)?
        .to_string();
    let invitation = SecretBytes::new(Zeroizing::new(rest[split + 1..].to_vec()))?;
    bytes.zeroize();
    Ok((lobby_id, invitation))
}

fn assemble_invitation(
    lobby_id: &str,
    invitation: &SecretBytes,
) -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    LobbyId::parse(lobby_id).map_err(|_| NativeLobbyError::Route)?;
    let mut code = Zeroizing::new(Vec::with_capacity(
        JOIN_PREFIX.len() + 36 + 1 + invitation.as_bytes().len(),
    ));
    code.extend_from_slice(JOIN_PREFIX);
    code.extend_from_slice(lobby_id.as_bytes());
    code.push(b':');
    code.extend_from_slice(invitation.as_bytes());
    Ok(code)
}

#[cfg(target_os = "linux")]
fn native_clipboard_write(value: &[u8]) -> Result<(), NativeLobbyError> {
    // The OS clipboard and helper process are an explicit human-sharing
    // boundary. They cannot be promised zeroizable; no Godot String or
    // DisplayServer clipboard API is involved.
    for (program, arguments) in [
        ("wl-copy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
    ] {
        let Ok(mut child) = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut input| input.write_all(value).is_ok());
        if wrote && child.wait().is_ok_and(|status| status.success()) {
            return Ok(());
        }
    }
    Err(NativeLobbyError::Clipboard)
}

#[cfg(target_os = "linux")]
fn native_clipboard_read() -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    for (program, arguments) in [
        ("wl-paste", &["--no-newline"][..]),
        ("xclip", &["-selection", "clipboard", "-o"][..]),
    ] {
        let Ok(output) = Command::new(program).args(arguments).output() else {
            continue;
        };
        if output.status.success() {
            return Ok(Zeroizing::new(output.stdout));
        }
    }
    Err(NativeLobbyError::Clipboard)
}

#[cfg(target_os = "linux")]
fn native_clipboard_clear_if_matches(expected: &[u8]) -> Result<(), NativeLobbyError> {
    for (reader, read_arguments, clearer, clear_arguments) in [
        (
            "wl-paste",
            &["--no-newline"][..],
            "wl-copy",
            &["--clear"][..],
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-o"][..],
            "xclip",
            &["-selection", "clipboard"][..],
        ),
    ] {
        let Ok(output) = Command::new(reader).args(read_arguments).output() else {
            continue;
        };
        let actual = Zeroizing::new(output.stdout);
        if output.status.success() && actual.as_slice() == expected {
            let mut child = Command::new(clearer)
                .args(clear_arguments)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| NativeLobbyError::Clipboard)?;
            drop(child.stdin.take());
            return child
                .wait()
                .ok()
                .filter(std::process::ExitStatus::success)
                .map(|_| ())
                .ok_or(NativeLobbyError::Clipboard);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn native_clipboard_write(value: &[u8]) -> Result<(), NativeLobbyError> {
    write_command_stdin("pbcopy", &[], value)
}

#[cfg(target_os = "macos")]
fn native_clipboard_read() -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    let output = Command::new("pbpaste")
        .output()
        .map_err(|_| NativeLobbyError::Clipboard)?;
    output
        .status
        .success()
        .then(|| Zeroizing::new(output.stdout))
        .ok_or(NativeLobbyError::Clipboard)
}

#[cfg(target_os = "macos")]
fn native_clipboard_clear_if_matches(expected: &[u8]) -> Result<(), NativeLobbyError> {
    if native_clipboard_read()?.as_slice() == expected {
        write_command_stdin("pbcopy", &[], &[])?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
const POWERSHELL_CLIPBOARD_WRITE: &str = "[Console]::In.ReadToEnd() | Set-Clipboard";

#[cfg(target_os = "windows")]
fn native_clipboard_write(value: &[u8]) -> Result<(), NativeLobbyError> {
    write_command_stdin(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            POWERSHELL_CLIPBOARD_WRITE,
        ],
        value,
    )
}

#[cfg(target_os = "windows")]
fn native_clipboard_read() -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Out.Write((Get-Clipboard -Raw))",
        ])
        .output()
        .map_err(|_| NativeLobbyError::Clipboard)?;
    output
        .status
        .success()
        .then(|| Zeroizing::new(output.stdout))
        .ok_or(NativeLobbyError::Clipboard)
}

#[cfg(target_os = "windows")]
fn native_clipboard_clear_if_matches(expected: &[u8]) -> Result<(), NativeLobbyError> {
    if native_clipboard_read()?.as_slice() == expected {
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Set-Clipboard -Value $null",
            ])
            .status()
            .map_err(|_| NativeLobbyError::Clipboard)?;
        if !status.success() {
            return Err(NativeLobbyError::Clipboard);
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn write_command_stdin(
    program: &str,
    arguments: &[&str],
    value: &[u8],
) -> Result<(), NativeLobbyError> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| NativeLobbyError::Clipboard)?;
    child
        .stdin
        .take()
        .ok_or(NativeLobbyError::Clipboard)?
        .write_all(value)
        .map_err(|_| NativeLobbyError::Clipboard)?;
    child
        .wait()
        .ok()
        .filter(std::process::ExitStatus::success)
        .map(|_| ())
        .ok_or(NativeLobbyError::Clipboard)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn native_clipboard_write(_value: &[u8]) -> Result<(), NativeLobbyError> {
    Err(NativeLobbyError::Clipboard)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn native_clipboard_read() -> Result<Zeroizing<Vec<u8>>, NativeLobbyError> {
    Err(NativeLobbyError::Clipboard)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn native_clipboard_clear_if_matches(_expected: &[u8]) -> Result<(), NativeLobbyError> {
    Ok(())
}

/// Rust-backed masked input. It stores no Godot text property and emits no
/// secret-valued signal. IME/composition, selection, undo, drag/drop and
/// context-menu text paths do not exist on this Control.
#[derive(GodotClass)]
#[class(base = Control)]
pub struct NativeSecretInput {
    #[base]
    base: Base<Control>,
    bytes: Zeroizing<Vec<u8>>,
    armed: bool,
}

impl NativeSecretInput {
    pub(crate) fn consume(&mut self) -> Result<SecretBytes, NativeLobbyError> {
        self.armed = false;
        SecretBytes::new(Zeroizing::new(std::mem::take(&mut *self.bytes)))
    }

    pub(crate) fn consume_join_code(&mut self) -> Result<(String, SecretBytes), NativeLobbyError> {
        self.armed = false;
        parse_join_code(Zeroizing::new(std::mem::take(&mut *self.bytes)))
    }
}

#[godot_api]
impl NativeSecretInput {
    #[func]
    pub(crate) fn arm_capture(&mut self) {
        self.bytes.zeroize();
        self.bytes.clear();
        self.armed = true;
        self.base_mut().grab_focus();
        self.base_mut().queue_redraw();
    }

    #[func]
    pub(crate) fn clear_capture(&mut self) {
        self.bytes.zeroize();
        self.bytes.clear();
        self.armed = false;
        self.base_mut().queue_redraw();
    }

    #[func]
    fn capture_ready(&self) -> bool {
        self.bytes.len() >= 32
    }
}

#[godot_api]
impl IControl for NativeSecretInput {
    fn init(base: Base<Control>) -> Self {
        Self {
            base,
            bytes: Zeroizing::new(Vec::with_capacity(640)),
            armed: true,
        }
    }

    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if !self.armed {
            return;
        }
        let Ok(key) = event.try_cast::<InputEventKey>() else {
            return;
        };
        if !key.is_pressed() || key.is_echo() || key.is_alt_pressed() {
            return;
        }
        let unicode = key.get_unicode();
        let paste_requested =
            (key.is_ctrl_pressed() || key.is_meta_pressed()) && matches!(unicode, 22 | 86 | 118);
        if paste_requested {
            if let Ok(mut pasted) = native_clipboard_read() {
                while pasted.first().is_some_and(u8::is_ascii_whitespace) {
                    pasted.remove(0);
                }
                while pasted.last().is_some_and(u8::is_ascii_whitespace) {
                    pasted.pop();
                }
                if pasted.len() <= 640
                    && pasted.iter().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b':' | b'.')
                    })
                {
                    let _ = native_clipboard_clear_if_matches(&pasted);
                    self.bytes.zeroize();
                    self.bytes = pasted;
                }
            }
            self.base_mut().accept_event();
            self.base_mut().queue_redraw();
            return;
        }
        if key.is_ctrl_pressed() || key.is_meta_pressed() {
            return;
        }
        if unicode == 8 || unicode == 127 {
            if let Some(last) = self.bytes.last_mut() {
                last.zeroize();
            }
            self.bytes.pop();
        } else if let Ok(byte) = u8::try_from(unicode) {
            if self.bytes.len() < 640
                && (byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
            {
                self.bytes.push(byte);
            }
        }
        self.base_mut().accept_event();
        self.base_mut().queue_redraw();
    }
}

pub(crate) fn safe_error() -> &'static str {
    SAFE_ERROR
}

pub(crate) fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(crate) fn route_for(operation: LobbyOperation, lobby_id: &str) -> Route<'_> {
    match operation {
        LobbyOperation::Lobby => Route::Lobby(lobby_id),
        LobbyOperation::Network => Route::Network(lobby_id),
        LobbyOperation::Endpoint => Route::Endpoint(lobby_id),
        LobbyOperation::Report => Route::Report(lobby_id),
        LobbyOperation::Start => Route::Start(lobby_id),
        LobbyOperation::Heartbeat => Route::Heartbeat(lobby_id),
        LobbyOperation::Leave => Route::Leave(lobby_id),
        LobbyOperation::End => Route::End(lobby_id),
        LobbyOperation::Readiness => Route::Readiness,
        LobbyOperation::Create => Route::Create,
        LobbyOperation::CreatorJoin | LobbyOperation::Share => Route::Invitation(lobby_id),
        LobbyOperation::Join => Route::Join(lobby_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn exact_origin_rejects_every_alternate_shape() {
        assert!(exact_url("/v1/capabilities").is_ok());
        for path in [
            "http://spurfire.rajsingh.info/v1/capabilities",
            "https://user@spurfire.rajsingh.info/v1/capabilities",
            "https://spurfire.rajsingh.info:444/v1/capabilities",
            "//evil.example/v1/capabilities",
            "/v1/capabilities?x=1",
            "/v1/capabilities#x",
            "/v1/../admin",
            "/v1/%2e%2e/admin",
            "/v1\\admin",
        ] {
            assert!(exact_url(path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn secret_debug_and_error_are_redacted() {
        let secret = SecretBytes::new(Zeroizing::new(
            b"abcdefghijklmnopqrstuvwxyzABCDEFGH".to_vec(),
        ))
        .unwrap();
        let debug = format!("{secret:?} {:?}", NativeLobbyError::Secret);
        assert!(!debug.contains("abcdefghijklmnopqrstuvwxyz"));
        assert_eq!(debug, "SecretBytes(<redacted>) secret");
    }

    #[test]
    fn parser_rejects_duplicate_escaped_malformed_and_dry_run_values() {
        let valid = b"abcdefghijklmnopqrstuvwxyzABCDEFGH";
        let mut duplicate = Zeroizing::new(
            format!(
                r#"{{"token":"{}","token":"{}"}}"#,
                String::from_utf8_lossy(valid),
                String::from_utf8_lossy(valid)
            )
            .into_bytes(),
        );
        assert!(extract_secret(&mut duplicate, b"token").is_err());
        let mut escaped =
            Zeroizing::new(br#"{"token":"abcdefghijklmnopqrstuvwxyzABCDE\u0046GH"}"#.to_vec());
        assert!(extract_secret(&mut escaped, b"token").is_err());
        let mut malformed = Zeroizing::new(br#"{"token":12}"#.to_vec());
        assert!(extract_secret(&mut malformed, b"token").is_err());
        let dry =
            SecretBytes::new(Zeroizing::new(b"DRY_RUN_NO_KEY__________________".to_vec())).unwrap();
        assert_ne!(dry.as_bytes(), b"DRY_RUN_NO_KEY");
    }

    #[test]
    fn declared_and_streamed_limits_are_exact() {
        assert_eq!(MAX_BODY, 65_536);
        let mut bounded = Zeroizing::new(Vec::with_capacity(MAX_BODY));
        bounded.extend(std::iter::repeat_n(0_u8, MAX_BODY));
        assert_eq!(bounded.len(), MAX_BODY);
        assert!(bounded.len().saturating_add(1) > MAX_BODY);
    }

    #[test]
    fn join_code_never_uses_a_godot_string() {
        let code = Zeroizing::new(
            b"SPURFIRE1:00000000-0000-4000-8000-000000000099:abcdefghijklmnopqrstuvwxyzABCDEFGH"
                .to_vec(),
        );
        let (lobby, invitation) = parse_join_code(code).unwrap();
        assert_eq!(lobby, "00000000-0000-4000-8000-000000000099");
        assert_eq!(invitation.as_bytes(), b"abcdefghijklmnopqrstuvwxyzABCDEFGH");
    }

    struct DropCanary(Arc<AtomicUsize>);
    impl Drop for DropCanary {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancellation_and_failure_drop_owned_canaries() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let _connect = DropCanary(drops.clone());
            let _body = DropCanary(drops.clone());
            let _parse = DropCanary(drops.clone());
            let _worker_send = DropCanary(drops.clone());
        }
        assert_eq!(drops.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn state_cancel_invalidates_events_and_drops_capabilities() {
        let mut state = LobbyClientState::default();
        let old = state.generation();
        state.install_creator(
            SecretBytes::new(Zeroizing::new(
                b"abcdefghijklmnopqrstuvwxyzABCDEFGH".to_vec(),
            ))
            .unwrap(),
        );
        state.cancel();
        assert_ne!(state.generation(), old);
        assert!(!state.has_creator());
        assert!(!state.has_participant());
    }
}
