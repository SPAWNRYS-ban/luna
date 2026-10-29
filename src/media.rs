//! One persistent speech subscription, invocation-scoped consumers and claims.
use super::*;
use std::collections::VecDeque;
use tokio::sync::Notify;

const CLAIM_TTL: Duration = Duration::from_millis(1500);
const POLL_TIME: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, Serialize)]
pub(super) struct Event {
    pub phase: String,
    pub request_id: String,
    pub transcript: String,
    pub r#final: bool,
    pub initial_snapshot: bool,
    pub source: &'static str,
    pub event_id: u64,
    #[serde(skip)]
    pub quiet: bool,
    #[serde(skip)]
    at: Instant,
}
impl Event {
    fn phase(phase: &str) -> Self {
        Self {
            phase: phase.into(),
            request_id: String::new(),
            transcript: String::new(),
            r#final: false,
            initial_snapshot: false,
            source: "voice",
            event_id: 0,
            quiet: phase == "idle",
            at: Instant::now(),
        }
    }
}
pub(super) fn decode(frame: &[u8]) -> Result<Option<Event>> {
    let frame = normalize_ipc_message(frame)?;
    if let Some(state) = nested_field(&frame, 3)? {
        let mut event = Event::phase(alice_state_phase(&state));
        event.quiet = matches!(varint_field(&state, 1)?, Some(0 | 6));
        event.request_id = text_field(&state, 7)?.unwrap_or_default();
        if let Some(asr) = nested_field(&state, 15)? {
            event.transcript = text_field(&asr, 1)?.unwrap_or_default();
            event.r#final = varint_field(&asr, 2)? == Some(1) && varint_field(&asr, 4)? == Some(1);
        }
        if let Some(source) = nested_field(&state, 10)? {
            if text_field(&source, 6)?.as_deref() == Some("command") {
                event.source = "command";
            }
        }
        if event.transcript.len() > 4096 || event.request_id.len() > 256 {
            bail!("speech event exceeds limit");
        }
        return Ok(Some(event));
    }
    // Spotter events without request IDs can be observed, but cannot cancel
    // or claim an unrelated native dialog.
    if let Some(event) = decode_ipc_event("aliced", &frame)? {
        if event["source"] == "command" {
            let mut value = Event::phase("recognition");
            value.transcript = event["transcript"].as_str().unwrap_or_default().into();
            value.r#final = true;
            value.source = "command";
            return Ok(Some(value));
        }
    }
    Ok(None)
}
struct Subscriber {
    token: String,
    cursor: u64,
    polling: Arc<AtomicBool>,
}
struct Utterance {
    event: Event,
    claimed: bool,
    closed: bool,
}
#[derive(Clone)]
struct Receipt {
    owner: String,
    token: String,
    epoch: u64,
    expires: Instant,
}
struct Hub {
    connected: bool,
    primed: bool,
    phase: String,
    sequence: u64,
    events: VecDeque<Event>,
    current: Option<Utterance>,
    subscribers: HashMap<String, Subscriber>,
    operations: HashMap<String, (String, Arc<AtomicBool>)>,
    receipts: HashMap<String, Receipt>,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            connected: false,
            primed: false,
            phase: "unknown".into(),
            sequence: 0,
            events: VecDeque::new(),
            current: None,
            subscribers: HashMap::new(),
            operations: HashMap::new(),
            receipts: HashMap::new(),
        }
    }
}
pub(super) struct Runtime {
    inner: std::sync::Mutex<Hub>,
    changed: Notify,
    pub control: Mutex<()>,
    backend: Option<native::Backend>,
    pub epoch: AtomicU64,
    stopping: AtomicBool,
}
impl Runtime {
    pub fn new(protocol: Option<SpeechProtocol>) -> Self {
        Self {
            inner: std::sync::Mutex::new(Hub::default()),
            changed: Notify::new(),
            control: Mutex::new(()),
            backend: protocol.map(|protocol| native::Backend {
                protocol,
                endpoint: None,
            }),
            epoch: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
        }
    }
    fn ingest(&self, mut event: Event) {
        let mut hub = self.inner.lock().unwrap();
        hub.connected = true;
        if event.initial_snapshot {
            hub.primed = event.quiet;
        }
        if event.quiet {
            hub.primed = true;
        }
        let new_request = !event.request_id.is_empty()
            && (event.phase == "listening" || !event.transcript.is_empty())
            && hub
                .current
                .as_ref()
                .is_none_or(|old| old.event.request_id != event.request_id);
        if new_request || (event.phase == "listening" && hub.phase != "listening") {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        if event.phase != "recognition" {
            hub.phase = event.phase.clone();
        }
        hub.sequence += 1;
        event.event_id = hub.sequence;
        if new_request {
            hub.current = Some(Utterance {
                event: event.clone(),
                claimed: event.initial_snapshot || !hub.primed,
                closed: false,
            });
        }
        if let Some(current) = hub.current.as_mut() {
            if event.initial_snapshot {
                current.claimed = true;
            }
            if matches!(event.phase.as_str(), "idle" | "unknown") {
                current.closed = true;
            }
            if event.request_id == current.event.request_id
                && event.r#final
                && !current.event.r#final
            {
                current.event = event.clone();
            }
        }
        hub.events.push_back(event);
        while hub.events.len() > 64 {
            hub.events.pop_front();
        }
        hub.receipts
            .retain(|_, receipt| receipt.expires > Instant::now());
        drop(hub);
        self.changed.notify_waiters();
    }
    fn disconnect(&self) {
        let mut hub = self.inner.lock().unwrap();
        hub.connected = false;
        hub.primed = false;
        hub.phase = "unknown".into();
        hub.current = None;
        hub.events.clear();
        hub.receipts.clear();
        self.epoch.fetch_add(1, Ordering::SeqCst);
        drop(hub);
        self.changed.notify_waiters();
    }
    pub fn revoke(&self, id: &str, token: &str) {
        let mut hub = self.inner.lock().unwrap();
        if hub.subscribers.get(id).is_some_and(|s| s.token == token) {
            hub.subscribers.remove(id);
        }
        if let Some((_, cancel)) = hub.operations.get(id).filter(|(owner, _)| owner == token) {
            cancel.store(true, Ordering::SeqCst);
        }
        hub.receipts
            .retain(|_, receipt| receipt.owner != id || receipt.token != token);
        drop(hub);
        self.changed.notify_waiters();
    }
    async fn next(&self, id: &str, token: &str) -> Result<Option<Event>> {
        let polling = {
            let mut hub = self.inner.lock().unwrap();
            let sequence = hub.sequence;
            let subscriber = hub
                .subscribers
                .entry(id.into())
                .or_insert_with(|| Subscriber {
                    token: token.into(),
                    cursor: sequence,
                    polling: Arc::new(AtomicBool::new(false)),
                });
            if subscriber.token != token {
                *subscriber = Subscriber {
                    token: token.into(),
                    cursor: sequence,
                    polling: Arc::new(AtomicBool::new(false)),
                };
            }
            if subscriber.polling.swap(true, Ordering::SeqCst) {
                bail!("speech poll already in flight");
            }
            subscriber.polling.clone()
        };
        struct PollGuard(Arc<AtomicBool>);
        impl Drop for PollGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _guard = PollGuard(polling);
        let deadline = tokio::time::Instant::now() + POLL_TIME;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut hub = self.inner.lock().unwrap();
                let subscriber = hub
                    .subscribers
                    .get(id)
                    .filter(|s| s.token == token)
                    .context("invocation stopped")?;
                let next = hub
                    .events
                    .iter()
                    .find(|event| event.event_id > subscriber.cursor)
                    .cloned();
                if let Some(event) = next {
                    hub.subscribers.get_mut(id).unwrap().cursor = event.event_id;
                    return Ok(Some(event));
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(None);
            }
        }
    }
    fn reserve(&self, request_id: &str) -> Result<Event> {
        let mut hub = self.inner.lock().unwrap();
        let current = hub.current.as_mut().context("no final speech to claim")?;
        if current.closed
            || current.claimed
            || current.event.initial_snapshot
            || current.event.source != "voice"
            || !current.event.r#final
            || current.event.request_id != request_id
            || current.event.at.elapsed() > CLAIM_TTL
        {
            bail!("speech claim expired or already taken");
        }
        current.claimed = true;
        Ok(current.event.clone())
    }
    fn candidate(&self, request_id: &str) -> Result<Event> {
        let hub = self.inner.lock().unwrap();
        let current = hub.current.as_ref().context("no final speech to claim")?;
        if current.event.request_id != request_id {
            bail!("native request changed");
        }
        Ok(current.event.clone())
    }
    fn operation(self: &Arc<Self>, id: &str, token: &str) -> Result<Operation> {
        let mut hub = self.inner.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            bail!("speech runtime is stopping");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        hub.operations
            .insert(id.into(), (token.into(), cancelled.clone()));
        Ok(Operation {
            runtime: self.clone(),
            owner: id.into(),
            cancelled,
        })
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        for (_, cancelled) in self.inner.lock().unwrap().operations.values() {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
    pub async fn wait_stopped(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.inner.lock().unwrap().operations.is_empty() {
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
struct Operation {
    runtime: Arc<Runtime>,
    owner: String,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Operation {
    fn drop(&mut self) {
        let mut hub = self.runtime.inner.lock().unwrap();
        if hub
            .operations
            .get(&self.owner)
            .is_some_and(|(_, flag)| Arc::ptr_eq(flag, &self.cancelled))
        {
            hub.operations.remove(&self.owner);
        }
    }
}
pub(super) fn start(app: App) {
    tokio::spawn(async move {
        while !app.speech.stopping.load(Ordering::Relaxed) {
            let backend = app.speech.backend.unwrap_or(native::Backend {
                protocol: SpeechProtocol::Direct,
                endpoint: None,
            });
            let run = async {
                let mut connection = backend.connect().await?;
                connection.subscribe().await?;
                let mut initial = true;
                while !app.speech.stopping.load(Ordering::Relaxed) {
                    if let Some(frame) = connection.frame(Instant::now() + POLL_TIME).await? {
                        if let Some(mut event) = decode(&frame)? {
                            // Unrelated handshake frames do not consume the snapshot marker.
                            event.initial_snapshot = initial;
                            if event.phase != "recognition" { initial = false; }
                            app.focus.lock().unwrap().native(&event.phase);
                            let route=event.r#final && event.source == "voice" && !event.initial_snapshot && !event.request_id.is_empty();
                            app.speech.ingest(event.clone());
                            if route && shared_command(&event.transcript).is_some() && app.settings.lock().await.media.voice_controls {
                                let app=app.clone();
                                tokio::spawn(async move {
                                    if let Err(error)=route_shared(&app,&event).await { tracing::debug!(%error,"shared voice command was not handled"); }
                                });
                            }
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            }.await;
            if !app.speech.stopping.load(Ordering::Relaxed) {
                app.speech.disconnect();
            }
            app.focus.lock().unwrap().native("unknown");
            if run.is_err() {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
}

#[derive(Clone, Debug, Serialize)]
struct Matched {
    pattern: String,
    query: Option<String>,
    #[serde(skip)]
    rank: usize,
}
impl CommandPolicy {
    pub(super) fn validate(&self, has_commands: bool) -> Result<()> {
        let count = self.phrases.len() + self.patterns.len();
        if has_commands != (count > 0) {
            bail!("commands permission requires declared phrases or patterns");
        }
        if count > 8192 || self.exclude.len() > 8192 {
            bail!("too many command declarations");
        }
        for value in &self.phrases {
            if value.len() > 128
                || value.contains(['{', '}'])
                || normalize_command(value).is_empty()
            {
                bail!("invalid command phrase");
            }
        }
        for value in self.patterns.iter().chain(&self.exclude) {
            let prefix = value.strip_suffix(" {query}").unwrap_or(value);
            if value.len() > 128
                || prefix.contains(['{', '}'])
                || normalize_command(prefix).is_empty()
                || (self.patterns.contains(value) && !value.ends_with(" {query}"))
            {
                bail!("invalid command pattern");
            }
        }
        Ok(())
    }
    fn matched(&self, text: &str) -> Option<Matched> {
        let normalized = normalize_command(text);
        let text = normalized.strip_prefix("алиса ").unwrap_or(&normalized);
        let capture = |value: &str| -> Option<Matched> {
            if let Some(prefix) = value.strip_suffix(" {query}") {
                let prefix = normalize_command(prefix) + " ";
                let query = text.strip_prefix(&prefix)?.trim();
                if query.is_empty() {
                    return None;
                }
                Some(Matched {
                    pattern: value.into(),
                    query: Some(query.into()),
                    rank: prefix.len(),
                })
            } else if normalize_command(value) == text {
                Some(Matched {
                    pattern: value.into(),
                    query: None,
                    rank: 4096 + value.len(),
                })
            } else {
                None
            }
        };
        if self.exclude.iter().any(|value| capture(value).is_some()) {
            return None;
        }
        self.phrases
            .iter()
            .chain(&self.patterns)
            .filter_map(|value| capture(value))
            .max_by_key(|m| m.rank)
    }
}
#[derive(Clone, Copy)]
enum SharedCommand {
    Transport(providers::Action),
    NowPlaying,
}
fn shared_command(text: &str) -> Option<SharedCommand> {
    use providers::Action::*;
    let normalized = normalize_command(text);
    let text = normalized.strip_prefix("алиса ").unwrap_or(&normalized);
    Some(match text {
        "пауза" | "поставь на паузу" | "поставь музыку на паузу" => {
            SharedCommand::Transport(Pause)
        }
        "продолжи" | "продолжи музыку" | "продолжи воспроизведение" => {
            SharedCommand::Transport(Resume)
        }
        "останови музыку" | "выключи музыку" => {
            SharedCommand::Transport(Stop)
        }
        "следующий трек" | "следующая песня" => {
            SharedCommand::Transport(Next)
        }
        "предыдущий трек" | "предыдущая песня" => {
            SharedCommand::Transport(Previous)
        }
        "что сейчас играет" | "что играет" | "какая песня играет" => {
            SharedCommand::NowPlaying
        }
        _ => return None,
    })
}
struct SharedTarget {
    owner: String,
    token: String,
    command: SharedCommand,
    track: Option<providers::Track>,
}
async fn shared_target(a: &App, text: &str) -> Result<Option<SharedTarget>> {
    if !a.settings.lock().await.media.voice_controls || a.speech.backend.is_none() {
        return Ok(None);
    }
    let Some(command) = shared_command(text) else {
        return Ok(None);
    };
    let snapshot = providers::snapshot(a).await;
    let Some(owner) = snapshot["selected"].as_str() else {
        return Ok(None);
    };
    let Some(provider) = snapshot["providers"].as_array().and_then(|items| {
        items
            .iter()
            .find(|p| p["id"] == owner && p["available"] == true)
    }) else {
        return Ok(None);
    };
    let track = serde_json::from_value::<providers::Track>(provider["metadata"].clone()).ok();
    match command {
        SharedCommand::Transport(action) => {
            let action = serde_json::to_value(action)?;
            if !provider["actions"]
                .as_array()
                .is_some_and(|actions| actions.contains(&action))
            {
                return Ok(None);
            }
            if provider["state"] == "unknown" && action != "pause" {
                return Ok(None);
            }
        }
        SharedCommand::NowPlaying => {
            if track.is_none() || provider["state"] == "unknown" {
                return Ok(None);
            }
        }
    }
    let Some(token) = a.runtime_tokens.lock().await.get(owner).cloned() else {
        return Ok(None);
    };
    Ok(Some(SharedTarget {
        owner: owner.into(),
        token,
        command,
        track,
    }))
}
async fn route_shared(a: &App, event: &Event) -> Result<()> {
    let _control = a
        .speech
        .control
        .try_lock()
        .map_err(|_| anyhow!("native speech operation in progress"))?;
    let Some(target) = shared_target(a, &event.transcript).await? else {
        return Ok(());
    };
    let op = {
        let tokens = a.runtime_tokens.lock().await;
        if tokens.get(&target.owner) != Some(&target.token) {
            bail!("provider stopped");
        }
        a.speech.operation(&target.owner, &target.token)?
    };
    let event = a.speech.reserve(&event.request_id)?;
    let epoch = a.speech.epoch.load(Ordering::SeqCst);
    a.speech
        .backend
        .unwrap()
        .connect()
        .await?
        .cancel(&event.request_id, &op.cancelled, &a.speech.epoch, epoch)
        .await?;
    if op.cancelled.load(Ordering::SeqCst) || a.speech.epoch.load(Ordering::SeqCst) != epoch {
        bail!("voice command interrupted");
    }
    match target.command {
        SharedCommand::Transport(action) => {
            providers::confirmed_for(a, &target.owner, &target.token, action).await?
        }
        SharedCommand::NowPlaying => {
            if a.providers.lock().await.selected_id() != Some(target.owner.as_str()) {
                bail!("selected provider changed");
            }
            let track = target.track.unwrap();
            let text = if track.artists.is_empty() {
                track.title
            } else {
                format!("{}. {}", track.artists.join(", "), track.title)
            };
            let text: String = text.chars().take(1000).collect();
            let _focus = focus::SpeechFocus::acquire(&a.focus);
            providers::wait_for_duck(a).await;
            a.speech
                .backend
                .unwrap()
                .connect()
                .await?
                .say(&text, &op.cancelled, &a.speech.epoch, epoch)
                .await?;
        }
    }
    Ok(())
}
async fn winner(a: &App, text: &str) -> Result<Option<(String, Matched)>> {
    if shared_target(a, text).await?.is_some() {
        return Ok(Some((
            "@luna".into(),
            Matched {
                pattern: normalize_command(text),
                query: None,
                rank: usize::MAX,
            },
        )));
    }
    let tokens = a.runtime_tokens.lock().await.clone();
    let selected = a.providers.lock().await.selected_id().map(str::to_owned);
    let plugins = a.plugins.lock().await;
    let mut candidates = Vec::new();
    for id in tokens.keys() {
        let manifest = plugins.manifest(id)?;
        if ["commands", "ipc.speech"]
            .iter()
            .all(|p| manifest.permissions.iter().any(|v| v == p))
        {
            if let Some(matched) = manifest.commands.matched(text) {
                candidates.push((id.clone(), matched));
            }
        }
    }
    candidates.sort_by(|a, b| {
        (selected.as_ref() == Some(&b.0))
            .cmp(&(selected.as_ref() == Some(&a.0)))
            .then_with(|| b.1.rank.cmp(&a.1.rank))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(candidates.into_iter().next())
}
async fn operation(a: &App, id: &str, headers: &HeaderMap) -> Result<Operation> {
    let tokens = a.runtime_tokens.lock().await;
    let token = runtime_token(headers)?;
    if tokens.get(id).map(String::as_str) != Some(token) {
        bail!("invalid runtime token");
    }
    a.speech.operation(id, token)
}
async fn claim_inner(
    a: &App,
    id: &str,
    headers: &HeaderMap,
    request: &str,
) -> Result<(Event, Matched, String)> {
    let _control = a
        .speech
        .control
        .try_lock()
        .map_err(|_| anyhow!("native speech operation in progress"))?;
    let backend = a
        .speech
        .backend
        .context("native speech control is not supported on this station")?;
    let candidate = a.speech.candidate(request)?;
    let (owner, matched) = winner(a, &candidate.transcript)
        .await?
        .context("command is not declared")?;
    if owner != id {
        bail!("command belongs to another package");
    }
    let op = operation(a, id, headers).await?;
    let event = a.speech.reserve(request)?;
    let epoch = a.speech.epoch.load(Ordering::SeqCst);
    backend
        .connect()
        .await?
        .cancel(&event.request_id, &op.cancelled, &a.speech.epoch, epoch)
        .await?;
    if op.cancelled.load(Ordering::Relaxed) || a.speech.epoch.load(Ordering::SeqCst) != epoch {
        bail!("command interrupted");
    }
    let command_id = Uuid::new_v4().to_string();
    a.speech.inner.lock().unwrap().receipts.insert(
        command_id.clone(),
        Receipt {
            owner: id.into(),
            token: runtime_token(headers)?.into(),
            epoch,
            expires: Instant::now() + Duration::from_secs(30),
        },
    );
    Ok((event, matched, command_id))
}
async fn claim(
    a: &App,
    id: &str,
    headers: &HeaderMap,
    request: &str,
) -> Result<(Event, Matched, String)> {
    // Finish a claimed cancellation even if its HTTP caller disconnects.
    let (a, id, headers, request) = (
        a.clone(),
        id.to_owned(),
        headers.clone(),
        request.to_owned(),
    );
    tokio::spawn(async move { claim_inner(&a, &id, &headers, &request).await }).await?
}
pub async fn capabilities(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    runtime_permission(&a, &id, &headers, "exec").await?;
    let manifest = a.plugins.lock().await.manifest(&id)?;
    let has = |name: &str| manifest.permissions.iter().any(|p| p == name);
    let connected = a.speech.inner.lock().unwrap().connected;
    let supported = a.speech.backend.is_some();
    Ok(Json(
        serde_json::json!({"version":2,"speech":has("ipc.speech"),"speech_connected":connected,
        "commands":has("commands") && supported,"tts":has("speech.tts") && supported,
        "focus":has("media.focus"),"provider":has("media.provider"),"metadata":true,
        "mpris":a.mpris_connected.load(Ordering::SeqCst),
        "led":has("led"),"led_count":a.led.settings.lock().await.count}),
    ))
}
pub async fn speech(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<Option<Event>>, ApiError> {
    runtime_permission(&a, &id, &headers, "ipc.speech").await?;
    let event = a.speech.next(&id, runtime_token(&headers)?).await?;
    runtime_permission(&a, &id, &headers, "ipc.speech").await?;
    Ok(Json(event))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    request_id: String,
}
pub async fn cancel_command(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<Claim>,
) -> Result<StatusCode, ApiError> {
    runtime_permission(&a, &id, &headers, "commands").await?;
    runtime_permission(&a, &id, &headers, "ipc.speech").await?;
    claim(&a, &id, &headers, &req.request_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn command(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<Option<serde_json::Value>>, ApiError> {
    runtime_permission(&a, &id, &headers, "commands").await?;
    runtime_permission(&a, &id, &headers, "ipc.speech").await?;
    let event = a.speech.next(&id, runtime_token(&headers)?).await?;
    runtime_permission(&a, &id, &headers, "commands").await?;
    let Some(event) = event.filter(|e| {
        e.r#final
            && e.source == "voice"
            && !e.initial_snapshot
            && !e.request_id.is_empty()
            && e.at.elapsed() <= CLAIM_TTL
    }) else {
        return Ok(Json(None));
    };
    if winner(&a, &event.transcript)
        .await?
        .is_none_or(|(owner, _)| owner != id)
    {
        return Ok(Json(None));
    }
    let (event, matched, command_id) = claim(&a, &id, &headers, &event.request_id).await?;
    Ok(Json(Some(
        serde_json::json!({"id":command_id,"request_id":event.request_id,
        "transcript":event.transcript,"pattern":matched.pattern,"query":matched.query}),
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Say {
    text: String,
    #[serde(default)]
    command_id: Option<String>,
}
pub async fn say(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<Say>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Caller disconnects must not release ducking while native TTS is alive.
    tokio::spawn(async move { say_inner(a, id, headers, req).await }).await?
}
async fn say_inner(
    a: App,
    id: String,
    headers: HeaderMap,
    req: Say,
) -> Result<Json<serde_json::Value>, ApiError> {
    runtime_permission(&a, &id, &headers, "speech.tts").await?;
    if req.text.trim().is_empty()
        || req.text.chars().count() > 1000
        || req.text.chars().any(|c| c.is_control())
    {
        return Err(ApiError(anyhow!(
            "speech text must contain 1..1000 characters without controls"
        )));
    }
    let _control = a
        .speech
        .control
        .try_lock()
        .map_err(|_| anyhow!("native speech operation in progress"))?;
    let backend = a
        .speech
        .backend
        .context("native speech synthesis is not supported on this station")?;
    let op = operation(&a, &id, &headers).await?;
    let epoch = a.speech.epoch.load(Ordering::SeqCst);
    if let Some(command_id) = req.command_id {
        let hub = a.speech.inner.lock().unwrap();
        let receipt = hub
            .receipts
            .get(&command_id)
            .context("unknown command receipt")?;
        if receipt.owner != id
            || receipt.token != runtime_token(&headers)?
            || receipt.epoch != epoch
            || receipt.expires <= Instant::now()
        {
            return Err(ApiError(anyhow!(
                "command response expired or belongs to another invocation"
            )));
        }
    }
    let _focus = focus::SpeechFocus::acquire(&a.focus);
    // Give cooperative providers one poll interval to apply the transient gain.
    providers::wait_for_duck(&a).await;
    backend
        .connect()
        .await?
        .say(&req.text, &op.cancelled, &a.speech.epoch, epoch)
        .await?;
    Ok(Json(serde_json::json!({"completed":true})))
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
