//! One opt-in MPRIS object for the selected provider on the existing player bus.
use super::*;
use zbus::{
    connection::Builder,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NAME: &str = "org.mpris.MediaPlayer2.luna";
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    /// Explicitly opt in; Luna does not replace system Bluetooth configuration.
    #[serde(default)]
    pub mpris_bus: Option<String>,
    /// Shared voice transport is opt-in and requires a supported speech backend.
    #[serde(default)]
    pub voice_controls: bool,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if let Some(address) = &self.mpris_bus {
            let path = address
                .strip_prefix("unix:path=")
                .context("media.mpris_bus must name a local Unix socket")?;
            if !path.starts_with('/') || path.len() > 107 || path.contains([',', ';', '\0']) {
                bail!("invalid media.mpris_bus socket path");
            }
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Default)]
struct View {
    provider: Option<String>,
    available: bool,
    state: String,
    actions: Vec<String>,
    metadata: Option<providers::Track>,
    position_ms: u64,
}
impl View {
    fn from_snapshot(snapshot: &serde_json::Value) -> Self {
        let Some(selected) = snapshot["selected"].as_str() else {
            return Self::default();
        };
        let Some(provider) = snapshot["providers"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == selected))
        else {
            return Self::default();
        };
        let available = provider["available"] == true;
        Self {
            provider: Some(selected.into()),
            available,
            state: provider["state"].as_str().unwrap_or("unknown").into(),
            actions: provider["actions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
            metadata: available
                .then(|| serde_json::from_value(provider["metadata"].clone()).ok())
                .flatten(),
            position_ms: if available {
                provider["position_ms"].as_u64().unwrap_or(0)
            } else {
                0
            },
        }
    }
    fn supports(&self, action: &str) -> bool {
        self.available && self.actions.iter().any(|a| a == action)
    }
    fn status(&self) -> &str {
        if !self.available {
            return "Stopped";
        }
        match self.state.as_str() {
            "playing" => "Playing",
            "paused" => "Paused",
            _ => "Stopped",
        }
    }
}
struct Root;
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    fn quit(&self) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Luna"
    }
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec![]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}
struct Player {
    app: App,
}
fn unsupported() -> zbus::fdo::Error {
    zbus::fdo::Error::NotSupported("Not supported by the selected provider".into())
}
impl Player {
    async fn view(&self) -> View {
        View::from_snapshot(&providers::snapshot(&self.app).await)
    }
    async fn transport(&self, action: providers::Action) -> zbus::fdo::Result<()> {
        providers::confirmed(&self.app, action)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn pause(&self) -> zbus::fdo::Result<()> {
        self.transport(providers::Action::Pause).await
    }
    async fn play(&self) -> zbus::fdo::Result<()> {
        self.transport(providers::Action::Resume).await
    }
    async fn play_pause(&self) -> zbus::fdo::Result<()> {
        providers::toggle(&self.app)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
    async fn stop(&self) -> zbus::fdo::Result<()> {
        self.transport(providers::Action::Stop).await
    }
    async fn next(&self) -> zbus::fdo::Result<()> {
        self.transport(providers::Action::Next).await
    }
    async fn previous(&self) -> zbus::fdo::Result<()> {
        self.transport(providers::Action::Previous).await
    }
    fn seek(&self, _offset: i64) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    fn set_position(&self, _track_id: OwnedObjectPath, _position: i64) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    fn open_uri(&self, _uri: &str) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    #[zbus(property)]
    async fn playback_status(&self) -> String {
        self.view().await.status().into()
    }
    #[zbus(property)]
    async fn metadata(&self) -> HashMap<String, OwnedValue> {
        let view = self.view().await;
        let Some(track) = view.metadata else {
            return HashMap::new();
        };
        let id = format!(
            "{PATH}/track/t{:x}",
            Sha256::digest(format!(
                "{}:{}",
                view.provider.unwrap_or_default(),
                track.id
            ))
        );
        let mut map = HashMap::new();
        map.insert(
            "mpris:trackid".into(),
            Value::from(OwnedObjectPath::try_from(id).unwrap())
                .try_into()
                .unwrap(),
        );
        map.insert(
            "mpris:length".into(),
            (track.duration_ms as i64 * 1000).into(),
        );
        map.insert(
            "xesam:title".into(),
            Value::from(track.title).try_into().unwrap(),
        );
        map.insert(
            "xesam:artist".into(),
            Value::from(track.artists).try_into().unwrap(),
        );
        if let Some(url) = track.artwork_url {
            map.insert("mpris:artUrl".into(), Value::from(url).try_into().unwrap());
        }
        map
    }
    #[zbus(property(emits_changed_signal = "false"))]
    async fn position(&self) -> i64 {
        self.view().await.position_ms as i64 * 1000
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn set_rate(&self, rate: f64) -> zbus::fdo::Result<()> {
        if rate == 1.0 {
            Ok(())
        } else {
            Err(unsupported())
        }
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    // Native volume continues through maind/ALSA. Transient gain is applied by
    // the provider, not this property (otherwise volume would be applied twice).
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn set_volume(&self, _volume: f64) -> zbus::fdo::Result<()> {
        Err(unsupported())
    }
    #[zbus(property)]
    async fn can_control(&self) -> bool {
        self.view().await.available
    }
    #[zbus(property)]
    async fn can_play(&self) -> bool {
        self.view().await.supports("resume")
    }
    #[zbus(property)]
    async fn can_pause(&self) -> bool {
        self.view().await.supports("pause")
    }
    #[zbus(property)]
    async fn can_go_next(&self) -> bool {
        self.view().await.supports("next")
    }
    #[zbus(property)]
    async fn can_go_previous(&self) -> bool {
        self.view().await.supports("previous")
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        false
    }
}
async fn session(app: App, address: &str) -> Result<()> {
    let connection = Builder::address(address)?
        .serve_at(PATH, Root)?
        .serve_at(PATH, Player { app: app.clone() })?
        .build()
        .await?;
    let owned = connection
        .request_name_with_flags(NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await?;
    if !matches!(
        owned,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    ) {
        bail!("MPRIS name is already in use");
    }
    struct Connected(Arc<AtomicBool>);
    impl Drop for Connected {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    app.mpris_connected.store(true, Ordering::SeqCst);
    let _connected = Connected(app.mpris_connected.clone());
    let interface = connection
        .object_server()
        .interface::<_, Player>(PATH)
        .await?;
    let mut previous = View::default();
    loop {
        if app.plugins.lock().await.shutting_down
            || app.settings.lock().await.media.mpris_bus.as_deref() != Some(address)
        {
            return Ok(());
        }
        if connection.is_closed() {
            bail!("MPRIS bus disconnected");
        }
        let current = View::from_snapshot(&providers::snapshot(&app).await);
        if current != previous {
            let player = interface.get().await;
            let emitter = interface.signal_emitter();
            if current.status() != previous.status() {
                player.playback_status_changed(emitter).await?;
            }
            if current.metadata != previous.metadata || current.provider != previous.provider {
                player.metadata_changed(emitter).await?;
            }
            if current.available != previous.available || current.actions != previous.actions {
                player.can_control_changed(emitter).await?;
                player.can_play_changed(emitter).await?;
                player.can_pause_changed(emitter).await?;
                player.can_go_next_changed(emitter).await?;
                player.can_go_previous_changed(emitter).await?;
            }
            previous = current;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
pub(super) fn start(app: App) {
    tokio::spawn(async move {
        loop {
            if app.plugins.lock().await.shutting_down {
                return;
            }
            let config = app.settings.lock().await.media.clone();
            if let Some(address) = config.mpris_bus {
                if let Err(error) = session(app.clone(), &address).await {
                    tracing::warn!(%error,"MPRIS bridge unavailable; retrying");
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}
