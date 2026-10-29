use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

fn frame_state(phase: u64, request: &str, text: &str, final_result: bool) -> Vec<u8> {
    let mut state = vec![8, phase as u8];
    push_bytes(&mut state, 7, request.as_bytes());
    let mut asr = Vec::new();
    push_bytes(&mut asr, 1, text.as_bytes());
    asr.extend_from_slice(&[16, u8::from(final_result), 32, 1]);
    push_bytes(&mut state, 15, &asr);
    let mut frame = Vec::new();
    push_bytes(&mut frame, 3, &state);
    frame
}
fn event(request: &str, text: &str, final_result: bool) -> Event {
    decode(&frame_state(1, request, text, final_result))
        .unwrap()
        .unwrap()
}
#[test]
fn claims_require_fresh_final_and_are_global_once_only() {
    let runtime = Runtime::new(None);
    let mut initial = event("old", "включи jazz", true);
    initial.initial_snapshot = true;
    runtime.ingest(initial);
    runtime.ingest(event("old", "включи jazz", true));
    assert!(runtime.reserve("old").is_err(), "replayed startup snapshot");
    runtime.ingest(Event::phase("idle"));
    runtime.ingest(event("new", "включи", false));
    assert!(runtime.reserve("new").is_err(), "partial was claimable");
    runtime.ingest(event("new", "включи jazz", true));
    assert!(runtime.reserve("wrong").is_err());
    assert!(
        runtime.reserve("new").is_ok(),
        "bad request consumed a valid claim"
    );
    assert!(runtime.reserve("new").is_err(), "duplicate claim");
    runtime.ingest(event("late", "включи jazz", true));
    runtime
        .inner
        .lock()
        .unwrap()
        .current
        .as_mut()
        .unwrap()
        .event
        .at = Instant::now() - Duration::from_secs(2);
    assert!(runtime.reserve("late").is_err());
    runtime.ingest(event("closed", "включи jazz", true));
    runtime.ingest(Event::phase("idle"));
    assert!(runtime.reserve("closed").is_err());
    runtime.ingest(event("disconnected", "включи jazz", true));
    runtime.disconnect();
    assert!(runtime.reserve("disconnected").is_err());
}
#[test]
fn native_tts_state_is_not_another_voice_request() {
    let runtime = Runtime::new(None);
    runtime.ingest(event("command", "что играет", true));
    let epoch = runtime.epoch.load(Ordering::SeqCst);
    let mut speaking = Event::phase("speaking");
    speaking.request_id = "native-tts".into();
    runtime.ingest(speaking);
    assert_eq!(runtime.epoch.load(Ordering::SeqCst), epoch);
}
#[test]
fn dynamic_patterns_exclusions_and_boundaries() {
    let policy = CommandPolicy {
        phrases: vec!["что играет".into()],
        patterns: vec!["включи {query}".into()],
        exclude: vec!["включи свет".into(), "включи таймер {query}".into()],
    };
    policy.validate(true).unwrap();
    assert_eq!(
        policy
            .matched("Алиса, включи Sewerslvt Newlove!")
            .unwrap()
            .query
            .as_deref(),
        Some("sewerslvt newlove")
    );
    assert!(policy.matched("включи").is_none());
    assert!(policy.matched("переключи jazz").is_none());
    assert!(policy.matched("включи свет").is_none());
    assert!(policy.matched("включи таймер на пять минут").is_none());
    assert!(policy.matched("Алиса, что играет?").is_some());
    for invalid in [
        "{query}",
        "включи {query} пожалуйста",
        "{foo} {query}",
        "включи",
    ] {
        let policy = CommandPolicy {
            patterns: vec![invalid.into()],
            ..Default::default()
        };
        assert!(policy.validate(true).is_err(), "{invalid}");
    }
    assert!(policy.validate(false).is_err());
}
#[tokio::test]
async fn poll_is_persistent_single_flight_and_revoked_on_restart() {
    let runtime = Runtime::new(None);
    runtime.ingest(event("already-spoken", "что играет", true));
    let first = runtime.next("p", "old");
    tokio::pin!(first);
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut first)
        .await
        .is_err());
    assert!(runtime.next("p", "old").await.is_err());
    runtime.ingest(event("fresh", "что играет", true));
    assert_eq!(first.await.unwrap().unwrap().request_id, "fresh");
    let waiting = runtime.next("p", "old");
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    runtime.revoke("p", "old");
    assert!(waiting.await.is_err());
}

async fn write_frame(stream: &mut tokio::net::TcpStream, frame: &[u8]) {
    stream
        .write_all(&(frame.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(frame).await.unwrap();
}
async fn read_frame(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let size = stream.read_u32_le().await.ok()?;
    if size > 1_048_576 {
        return None;
    }
    let mut frame = vec![0; size as usize];
    stream.read_exact(&mut frame).await.ok()?;
    Some(frame)
}
struct Mock {
    address: SocketAddr,
    state: Arc<std::sync::Mutex<Vec<u8>>>,
    frames: broadcast::Sender<Vec<u8>>,
    methods: Arc<std::sync::Mutex<Vec<u64>>>,
    tts_error: Arc<AtomicBool>,
    hang_tts: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn start(protocol: SpeechProtocol) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(std::sync::Mutex::new(frame_state(0, "", "", false)));
        let (frames, _) = broadcast::channel::<Vec<u8>>(32);
        let methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tts_error = Arc::new(AtomicBool::new(false));
        let hang_tts = Arc::new(AtomicBool::new(false));
        let (initial, events, received, fail, hang) = (
            state.clone(),
            frames.clone(),
            methods.clone(),
            tts_error.clone(),
            hang_tts.clone(),
        );
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let (initial, events, received, fail, hang) = (
                    initial.clone(),
                    events.clone(),
                    received.clone(),
                    fail.clone(),
                    hang.clone(),
                );
                tokio::spawn(async move {
                    let mut rx = events.subscribe();
                    let snapshot = initial.lock().unwrap().clone();
                    write_frame(&mut stream, &snapshot).await;
                    loop {
                        tokio::select! {
                            event=rx.recv()=>match event { Ok(frame)=>write_frame(&mut stream,&frame).await,Err(_)=>return },
                            request=read_frame(&mut stream)=>{
                                let Some(request)=request else { return; };
                                // Assert the actual firmware envelope, not only a decoded method.
                                match protocol {
                                    SpeechProtocol::Envelope=>assert_eq!(varint_field(&request,253).unwrap(),Some(210)),
                                    SpeechProtocol::Direct=>assert!(nested_field(&request,210).unwrap().is_some()),
                                }
                                let normalized=normalize_ipc_message(&request).unwrap();
                                let remoting=nested_field(&normalized,210).unwrap().unwrap();
                                let Some(method)=nested_field(&remoting,8).unwrap() else { continue; };
                                let id=varint_field(&method,1).unwrap().unwrap();
                                received.lock().unwrap().push(id);
                                match id {
                                    1|3=>{
                                        let idle=frame_state(0,"","",false);
                                        *initial.lock().unwrap()=idle.clone();
                                        let _=events.send(idle);
                                    }
                                    18=>{
                                        let text=nested_field(&method,13).unwrap().unwrap();
                                        assert!(!text_field(&text,1).unwrap().unwrap().is_empty());
                                        let request_id=text_field(&method,10).unwrap().unwrap();
                                        let notification=|kind|{
                                            let mut event=vec![8,kind];
                                            push_bytes(&mut event,11,request_id.as_bytes());
                                            let mut remoting=Vec::new();push_bytes(&mut remoting,18,&event);
                                            let mut frame=Vec::new();push_bytes(&mut frame,210,&remoting);frame
                                        };
                                        // A stale completion before STARTED must not complete this request.
                                        write_frame(&mut stream,&notification(1)).await;
                                        write_frame(&mut stream,&notification(6)).await;
                                        if !hang.load(Ordering::SeqCst) {
                                            tokio::time::sleep(Duration::from_millis(30)).await;
                                            write_frame(&mut stream,&notification(if fail.load(Ordering::SeqCst) {4} else {1})).await;
                                        }
                                    }
                                    14=>(),
                                    _=>panic!("unexpected native method {id}"),
                                }
                            }
                        }
                    }
                });
            }
        });
        Self {
            address,
            state,
            frames,
            methods,
            tts_error,
            hang_tts,
            task,
        }
    }
    fn utterance(&self, request: &str, text: &str) {
        let frame = frame_state(1, request, text, true);
        *self.state.lock().unwrap() = frame.clone();
        self.frames.send(frame).unwrap();
    }
}
async fn fixture(protocol: SpeechProtocol) -> (App, Mock) {
    let root = std::env::temp_dir().join(format!("luna-speech-{}", Uuid::new_v4()));
    for id in ["alpha", "beta"] {
        let package = root.join("plugins").join(id);
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("plugin.toml"),
            format!(
                r#"id="{id}"
name="{id}"
version="1"
entry="run.sh"
permissions=["exec","ipc.speech","commands","speech.tts","media.focus","media.provider"]
[media_provider]
label="Player"
actions=["pause","resume","stop","next","previous"]
ducking=true
[commands]
phrases=["что играет"]
patterns=["включи {{query}}"]
exclude=["включи свет"]
"#
            ),
        )
        .unwrap();
    }
    let mock = Mock::start(protocol).await;
    let mut runtime = Runtime::new(Some(protocol));
    runtime.backend.as_mut().unwrap().endpoint = Some(mock.address);
    let app = App {
        root: root.clone(),
        settings: Arc::new(Mutex::new(Settings::default())),
        device: Arc::new(Mutex::new(DeviceProfile {
            model: "test".into(),
            architecture: "x86_64".into(),
            features: vec![],
            source: "test".into(),
            led: LedTopology::default(),
        })),
        led: Arc::new(Led {
            settings: Arc::new(Mutex::new(LedSettings::default())),
            backend: None,
            color_order: ColorOrder::Rgb,
            topology: LedTopology::default(),
        }),
        microphone: None,
        clock: None,
        plugins: Arc::new(Mutex::new(
            PluginManager::new(root.join("plugins"), None, None).unwrap(),
        )),
        runtime_tokens: Arc::new(Mutex::new(HashMap::from([
            ("alpha".into(), "a1".into()),
            ("beta".into(), "b1".into()),
        ]))),
        mpris_connected: Arc::new(AtomicBool::new(false)),
        speech: Arc::new(runtime),
        providers: Arc::new(Mutex::new(providers::Broker::default())),
        focus: Arc::new(std::sync::Mutex::new(focus::Focus::default())),
        custom_web: Arc::new(Mutex::new(HashMap::new())),
        runtime_api_url: "http://127.0.0.1:1".into(),
        input_actions: &[],
        input_gestures: &[],
        gpio_inputs: &[],
    };
    start(app.clone());
    wait(|| app.speech.inner.lock().unwrap().connected).await;
    (app, mock)
}
async fn wait(check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-luna-token", token.parse().unwrap());
    headers
}
#[tokio::test]
async fn routing_is_deterministic_and_only_winner_cancels() {
    let (app, mock) = fixture(SpeechProtocol::Envelope).await;
    mock.utterance("one", "Алиса, включи jazz");
    wait(|| {
        app.speech
            .inner
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|u| u.event.r#final)
    })
    .await;
    assert!(claim(&app, "beta", &headers("b1"), "one").await.is_err());
    assert!(mock.methods.lock().unwrap().is_empty());
    let token_headers = headers("a1");
    let (first, second) = tokio::join!(
        claim(&app, "alpha", &token_headers, "one"),
        claim(&app, "alpha", &token_headers, "one")
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        *mock.methods.lock().unwrap(),
        vec![3],
        "Lite inherited Mini cancellation"
    );
    mock.utterance("two", "включи свет");
    wait(|| {
        app.speech
            .inner
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|u| u.event.request_id == "two")
    })
    .await;
    assert!(claim(&app, "alpha", &headers("a1"), "two").await.is_err());
    assert_eq!(*mock.methods.lock().unwrap(), vec![3]);
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[tokio::test]
async fn mini_cancel_uses_its_own_wire_format() {
    let (app, mock) = fixture(SpeechProtocol::Direct).await;
    mock.utterance("one", "что играет");
    wait(|| {
        app.speech
            .inner
            .lock()
            .unwrap()
            .current
            .as_ref()
            .is_some_and(|u| u.event.r#final)
    })
    .await;
    claim(&app, "alpha", &headers("a1"), "one").await.unwrap();
    assert_eq!(*mock.methods.lock().unwrap(), vec![14, 3, 1]);
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[tokio::test]
async fn tts_confirms_completion_restores_focus_and_cancels_revoked_owner() {
    let (app, mock) = fixture(SpeechProtocol::Envelope).await;
    let speak = |app: App| {
        say_inner(
            app,
            "alpha".into(),
            headers("a1"),
            Say {
                text: "Включаю".into(),
                command_id: None,
            },
        )
    };
    let started = Instant::now();
    assert!(speak(app.clone()).await.is_ok());
    assert!(
        started.elapsed() >= Duration::from_millis(30),
        "accepted stale completion"
    );
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 1.0);
    mock.tts_error.store(true, Ordering::SeqCst);
    assert!(speak(app.clone()).await.is_err());
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 1.0);
    mock.hang_tts.store(true, Ordering::SeqCst);
    let task = tokio::spawn(speak(app.clone()));
    wait(|| {
        mock.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|&&m| m == 18)
            .count()
            == 3
    })
    .await;
    app.speech.revoke("alpha", "a1");
    assert!(task.await.unwrap().is_err());
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 1.0);
    wait(|| mock.methods.lock().unwrap().last() == Some(&14)).await;
    assert_eq!(mock.methods.lock().unwrap().last(), Some(&14));
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[tokio::test]
async fn tts_never_interrupts_an_unrelated_native_dialog() {
    let (app, mock) = fixture(SpeechProtocol::Envelope).await;
    mock.utterance("native", "какая погода");
    wait(|| app.speech.inner.lock().unwrap().phase == "listening").await;
    assert!(say_inner(
        app.clone(),
        "alpha".into(),
        headers("a1"),
        Say {
            text: "Привет".into(),
            command_id: None
        }
    )
    .await
    .is_err());
    assert!(mock.methods.lock().unwrap().is_empty());
    assert_eq!(app.focus.lock().unwrap().snapshot()["tts"], false);
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[test]
fn parser_exposes_only_request_fields_and_rejects_malformed() {
    let mut state = vec![8, 1];
    push_bytes(&mut state, 14, b"private account response");
    let mut frame = Vec::new();
    push_bytes(&mut frame, 3, &state);
    assert!(!serde_json::to_string(&decode(&frame).unwrap())
        .unwrap()
        .contains("private account"));
    assert!(decode(&[255]).is_err());
}

async fn register(app: &App, id: &str, token: &str) {
    providers::report(State(app.clone()),AxumPath(id.into()),headers(token),Json(serde_json::from_value(serde_json::json!({
        "state":"paused","metadata":{"id":"test","title":"Track","artists":["Artist"]},"applied_gain":0.2
    })).unwrap())).await.unwrap();
    let _ = providers::select(
        State(app.clone()),
        Json(serde_json::from_value(serde_json::json!({"provider":id})).unwrap()),
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn selected_provider_wins_a_matching_package_command() {
    let (app, _mock) = fixture(SpeechProtocol::Envelope).await;
    register(&app, "beta", "b1").await;
    assert_eq!(
        winner(&app, "включи jazz").await.unwrap().unwrap().0,
        "beta"
    );
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[tokio::test]
async fn shared_voice_transport_and_title_go_through_one_owner() {
    let (app, mock) = fixture(SpeechProtocol::Envelope).await;
    register(&app, "alpha", "a1").await;
    app.settings.lock().await.media.voice_controls = true;
    let player = app.clone();
    let adapter = tokio::spawn(async move {
        loop {
            if let Some(command) = providers::poll(
                State(player.clone()),
                AxumPath("alpha".into()),
                headers("a1"),
            )
            .await
            .unwrap()
            .0
            {
                let command = serde_json::to_value(command).unwrap();
                assert_eq!(command["action"], "next");
                providers::result(
                    State(player.clone()),
                    AxumPath("alpha".into()),
                    headers("a1"),
                    Json(
                        serde_json::from_value(serde_json::json!({
                            "id":command["id"],"success":true,"state":"paused"
                        }))
                        .unwrap(),
                    ),
                )
                .await
                .unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    mock.utterance("next", "следующий трек");
    tokio::time::timeout(Duration::from_secs(3), adapter)
        .await
        .unwrap()
        .unwrap();
    wait(|| app.speech.control.try_lock().is_ok()).await;
    assert_eq!(*mock.methods.lock().unwrap(), vec![3]);
    mock.utterance("title", "что сейчас играет");
    wait(|| mock.methods.lock().unwrap().contains(&18)).await;
    wait(|| app.speech.control.try_lock().is_ok()).await;
    assert_eq!(*mock.methods.lock().unwrap(), vec![3, 3, 18]);
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 1.0);
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[tokio::test]
async fn runtime_http_checks_tokens_and_actual_command_delivery() {
    let (app, mock) = fixture(SpeechProtocol::Envelope).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = media_routes().with_state(app.clone());
    let http = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let url = format!("{base}/api/runtime/alpha/commands/next");
    assert_eq!(
        client.post(&url).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .post(&url)
            .header("X-Luna-Token", "b1")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let polling = tokio::spawn({
        let client = client.clone();
        let url = url.clone();
        async move {
            client
                .post(url)
                .header("X-Luna-Token", "a1")
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    });
    wait(|| {
        app.speech
            .inner
            .lock()
            .unwrap()
            .subscribers
            .contains_key("alpha")
    })
    .await;
    mock.utterance("http", "включи jazz");
    let response = polling.await.unwrap();
    assert_eq!(response["query"], "jazz");
    let say_url = format!("{base}/api/runtime/alpha/speech/say");
    let response = client
        .post(&say_url)
        .header("X-Luna-Token", "a1")
        .json(&serde_json::json!({
            "text":"Включаю","command_id":response["id"]
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(*mock.methods.lock().unwrap(), vec![3, 18]);
    assert_eq!(
        client
            .post(&say_url)
            .header("X-Luna-Token", "a1")
            .json(&serde_json::json!({
                "text":"Привет","command_id":"unknown"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    http.abort();
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}

#[tokio::test]
async fn focus_http_rejects_foreign_release_and_revoked_poll() {
    let (app, _mock) = fixture(SpeechProtocol::Envelope).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = media_routes().with_state(app.clone());
    let http = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let lease = client
        .post(format!("{base}/api/runtime/alpha/focus/acquire"))
        .header("X-Luna-Token", "a1")
        .json(&serde_json::json!({"ttl_ms":5000}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(lease["lease"].is_string());
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 0.2);
    let body = serde_json::json!({"lease":lease["lease"]});
    assert_eq!(
        client
            .post(format!("{base}/api/runtime/beta/focus/release"))
            .header("X-Luna-Token", "b1")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let polling = tokio::spawn({
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .post(format!("{base}/api/runtime/alpha/speech/next"))
                .header("X-Luna-Token", "a1")
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    wait(|| {
        app.speech
            .inner
            .lock()
            .unwrap()
            .subscribers
            .contains_key("alpha")
    })
    .await;
    revoke_runtime(&app, "alpha", "a1").await;
    assert_eq!(polling.await.unwrap(), StatusCode::BAD_REQUEST);
    assert_eq!(app.focus.lock().unwrap().snapshot()["gain"], 1.0);
    http.abort();
    app.speech.shutdown();
    fs::remove_dir_all(&app.root).unwrap();
}
#[test]
fn unknown_schema_state_is_not_treated_as_confirmed_idle() {
    for (state, quiet) in [(0, true), (6, true), (99, false), (1, false)] {
        assert_eq!(
            decode(&frame_state(state, "", "", false))
                .unwrap()
                .unwrap()
                .quiet,
            quiet
        );
    }
}

#[test]
fn local_fast_command_is_never_executed_again_as_voice() {
    let mut frame = frame_state(1, "local-command", "следующий трек", true);
    let mut body = nested_field(&frame, 3).unwrap().unwrap();
    let mut source = Vec::new();
    push_bytes(&mut source, 6, b"command");
    push_bytes(&mut body, 10, &source);
    frame.clear();
    push_bytes(&mut frame, 3, &body);
    let event = decode(&frame).unwrap().unwrap();
    assert_eq!(event.source, "command");
    let runtime = Runtime::new(None);
    runtime.ingest(event);
    assert!(runtime.reserve("local-command").is_err());
    let mut state = vec![8, 1];
    push_bytes(&mut state, 7, b"not-asr");
    let mut asr = Vec::new();
    push_bytes(&mut asr, 1, "следующий трек".as_bytes());
    asr.extend_from_slice(&[16, 1]);
    push_bytes(&mut state, 15, &asr);
    let mut frame = Vec::new();
    push_bytes(&mut frame, 3, &state);
    assert!(!decode(&frame).unwrap().unwrap().r#final);
}

#[test]
fn reconnect_during_listening_ignores_that_entire_turn() {
    let runtime = Runtime::new(None);
    let mut snapshot = Event::phase("listening");
    snapshot.initial_snapshot = true;
    runtime.ingest(snapshot);
    runtime.ingest(event("preexisting", "включи jazz", true));
    assert!(runtime.reserve("preexisting").is_err());
    runtime.ingest(Event::phase("idle"));
    runtime.ingest(event("new", "включи jazz", true));
    assert!(runtime.reserve("new").is_ok());
}

#[test]
fn shutdown_revokes_active_speech_and_rejects_late_operations() {
    let runtime = Arc::new(Runtime::new(None));
    let active = runtime.operation("p", "token").unwrap();
    runtime.shutdown();
    assert!(active.cancelled.load(Ordering::SeqCst));
    assert!(runtime.operation("p", "token").is_err());
    drop(active);
    assert!(runtime.inner.lock().unwrap().operations.is_empty());
}
