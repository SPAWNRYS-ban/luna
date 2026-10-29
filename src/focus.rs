//! Cooperative gain leases; never change the station's ALSA master volume.
use super::*;

struct Lease {
    owner: String,
    token: String,
    expires: Instant,
}
#[derive(Default)]
pub(super) struct Focus {
    native_dialog: bool,
    native_until: Option<Instant>,
    leases: HashMap<String, Lease>,
    tts: usize,
}
impl Focus {
    pub(super) fn native(&mut self, phase: &str) {
        if phase == "recognition" {
            return;
        }
        self.native_dialog = matches!(phase, "listening" | "busy" | "speaking" | "shazam");
        self.native_until = self
            .native_dialog
            .then(|| Instant::now() + Duration::from_secs(30));
    }
    pub(super) fn snapshot(&mut self) -> serde_json::Value {
        self.leases
            .retain(|_, lease| lease.expires > Instant::now());
        if self
            .native_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.native_dialog = false;
            self.native_until = None;
        }
        let active = self.native_dialog || self.tts > 0 || !self.leases.is_empty();
        serde_json::json!({"gain":if active {0.2} else {1.0},
            "native_dialog":self.native_dialog,"tts":self.tts>0,"lease_count":self.leases.len()})
    }
    fn acquire(&mut self, owner: &str, token: &str, ttl_ms: u64) -> Result<String> {
        if !(100..=30_000).contains(&ttl_ms) {
            bail!("focus TTL must be 100..30000 ms");
        }
        let _ = self.snapshot();
        if self.leases.len() >= 32 {
            bail!("too many focus leases");
        }
        let id = Uuid::new_v4().to_string();
        self.leases.insert(
            id.clone(),
            Lease {
                owner: owner.into(),
                token: token.into(),
                expires: Instant::now() + Duration::from_millis(ttl_ms),
            },
        );
        Ok(id)
    }
    pub(super) fn revoke(&mut self, owner: &str, token: &str) {
        self.leases
            .retain(|_, lease| lease.owner != owner || lease.token != token);
    }
}
pub(super) struct SpeechFocus(Arc<std::sync::Mutex<Focus>>);
impl SpeechFocus {
    pub(super) fn acquire(focus: &Arc<std::sync::Mutex<Focus>>) -> Self {
        focus.lock().unwrap().tts += 1;
        Self(focus.clone())
    }
}
impl Drop for SpeechFocus {
    fn drop(&mut self) {
        let mut focus = self.0.lock().unwrap();
        focus.tts = focus.tts.saturating_sub(1);
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Acquire {
    ttl_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Release {
    lease: String,
}
pub(super) async fn status(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    runtime_permission(&a, &id, &headers, "media.provider").await?;
    let mut focus = a.focus.lock().unwrap().snapshot();
    if a.providers.lock().await.selected_id() != Some(id.as_str()) {
        focus["gain"] = serde_json::json!(1.0);
    }
    Ok(Json(focus))
}
pub(super) async fn acquire(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<Acquire>,
) -> Result<Json<serde_json::Value>, ApiError> {
    runtime_permission(&a, &id, &headers, "media.focus").await?;
    let lease = a
        .focus
        .lock()
        .unwrap()
        .acquire(&id, runtime_token(&headers)?, req.ttl_ms)?;
    Ok(Json(serde_json::json!({"lease":lease})))
}
pub(super) async fn release(
    State(a): State<App>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    Json(req): Json<Release>,
) -> Result<StatusCode, ApiError> {
    runtime_permission(&a, &id, &headers, "media.focus").await?;
    let token = runtime_token(&headers)?;
    let mut focus = a.focus.lock().unwrap();
    let lease = focus
        .leases
        .get(&req.lease)
        .context("unknown focus lease")?;
    if lease.owner != id || lease.token != token {
        return Err(ApiError(anyhow!(
            "focus lease belongs to another invocation"
        )));
    }
    focus.leases.remove(&req.lease);
    Ok(StatusCode::NO_CONTENT)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_focus_and_owner_revocation_restore_gain() {
        let shared = Arc::new(std::sync::Mutex::new(Focus::default()));
        let tts = SpeechFocus::acquire(&shared);
        {
            let mut f = shared.lock().unwrap();
            f.acquire("a", "old", 1000).unwrap();
            f.acquire("b", "new", 1000).unwrap();
            f.native("listening");
            f.revoke("a", "old");
            assert_eq!(f.snapshot()["gain"], 0.2);
        }
        drop(tts);
        let mut f = shared.lock().unwrap();
        f.native("idle");
        assert_eq!(f.snapshot()["gain"], 0.2);
        f.revoke("b", "new");
        assert_eq!(f.snapshot()["gain"], 1.0);
    }
    #[test]
    fn expired_lease_does_not_hold_focus() {
        let mut f = Focus::default();
        let id = f.acquire("a", "token", 100).unwrap();
        f.leases.get_mut(&id).unwrap().expires = Instant::now();
        assert_eq!(f.snapshot()["gain"], 1.0);
    }
    #[test]
    fn missing_native_idle_cannot_hold_gain_forever() {
        let mut f = Focus::default();
        f.native("listening");
        f.native("recognition");
        assert_eq!(f.snapshot()["gain"], 0.2);
        f.native_until = Some(Instant::now());
        assert_eq!(f.snapshot()["gain"], 1.0);
        f.native("speaking");
        f.native("unknown");
        assert_eq!(f.snapshot()["gain"], 1.0);
    }
}
