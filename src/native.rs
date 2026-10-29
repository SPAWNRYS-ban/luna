//! Fixed, model-specific Alice IPC operations. No arbitrary remoting API.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as AsyncStream;

/// Wire formats verified independently on the first Mini and first Lite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechProtocol {
    Direct,
    Envelope,
}
#[derive(Clone, Copy)]
pub(super) struct Backend {
    pub protocol: SpeechProtocol,
    // Production resolves only aliced.port from the device's local config.
    pub endpoint: Option<SocketAddr>,
}
impl Backend {
    pub async fn connect(self) -> Result<Connection> {
        let endpoint = match self.endpoint {
            Some(address) => address,
            None => ipc_endpoint("aliced")?,
        };
        let stream =
            tokio::time::timeout(Duration::from_secs(1), AsyncStream::connect(endpoint)).await??;
        stream.set_nodelay(true)?;
        Ok(Connection {
            stream,
            buffered: Vec::new(),
            protocol: self.protocol,
        })
    }
}
pub(super) struct Connection {
    stream: AsyncStream,
    buffered: Vec<u8>,
    protocol: SpeechProtocol,
}
impl Connection {
    pub async fn frame(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>> {
        loop {
            if self.buffered.len() >= 4 {
                let size = ipc_frame_length(self.buffered[..4].try_into()?)?;
                if size > 2 * 1024 * 1024 {
                    bail!("native speech frame exceeds limit");
                }
                if self.buffered.len() >= size + 4 {
                    let frame = self.buffered[4..size + 4].to_vec();
                    self.buffered.drain(..size + 4);
                    return Ok(Some(frame));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let mut buffer = [0; 4096];
            match tokio::time::timeout(left, self.stream.read(&mut buffer)).await {
                Err(_) => return Ok(None),
                Ok(Ok(0)) => bail!("Alice IPC disconnected"),
                Ok(Ok(n)) => self.buffered.extend_from_slice(&buffer[..n]),
                Ok(Err(e)) => return Err(e.into()),
            }
            if self.buffered.len() > 2 * 1024 * 1024 + 4 {
                bail!("Alice IPC buffer exceeded");
            }
        }
    }
    async fn send(&mut self, field: u64, payload: &[u8]) -> Result<()> {
        let frame = remoting(self.protocol, field, payload);
        tokio::time::timeout(Duration::from_millis(500), async {
            self.stream
                .write_all(&(frame.len() as u32).to_le_bytes())
                .await?;
            self.stream.write_all(&frame).await
        })
        .await??;
        Ok(())
    }
    pub async fn subscribe(&mut self) -> Result<()> {
        let mut listener = Vec::new();
        push_bytes(&mut listener, 1, b"AliceCapability");
        self.send(20, &listener).await
    }
    async fn method(&mut self, method: u64) -> Result<()> {
        let mut payload = Vec::new();
        push_varint(&mut payload, 8);
        push_varint(&mut payload, method);
        self.send(8, &payload).await
    }
    async fn state(&mut self, deadline: Instant) -> Result<media::Event> {
        while let Some(frame) = self.frame(deadline).await? {
            if let Some(event) = media::decode(&frame)? {
                if event.phase == "unknown" && !event.quiet {
                    bail!("unsupported native speech state");
                }
                if event.phase != "recognition" {
                    return Ok(event);
                }
            }
        }
        bail!("Alice state was not confirmed")
    }
    pub async fn cancel(
        &mut self,
        request_id: &str,
        cancelled: &AtomicBool,
        epoch: &AtomicU64,
        expected: u64,
    ) -> Result<()> {
        self.subscribe().await?;
        let state = self
            .state(Instant::now() + Duration::from_millis(700))
            .await?;
        if cancelled.load(Ordering::Relaxed) {
            bail!("invocation stopped");
        }
        if state.request_id != request_id || state.quiet {
            bail!("native dialog changed before cancellation");
        }
        // The first Lite ignores the Mini's direct envelope and must never
        // inherit its three-method cancellation sequence.
        let methods: &[u64] = match self.protocol {
            SpeechProtocol::Envelope => &[3],
            SpeechProtocol::Direct => &[14, 3, 1],
        };
        for &method in methods {
            if cancelled.load(Ordering::SeqCst) || epoch.load(Ordering::SeqCst) != expected {
                bail!("native request changed before cancellation");
            }
            self.method(method).await?;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = self.state(deadline).await?;
            if cancelled.load(Ordering::Relaxed) {
                bail!("invocation stopped");
            }
            if !state.request_id.is_empty() && state.request_id != request_id {
                bail!("new native dialog started during cancellation");
            }
            if state.quiet {
                return Ok(());
            }
        }
    }
    pub async fn say(
        &mut self,
        text: &str,
        cancelled: &AtomicBool,
        epoch: &AtomicU64,
        expected: u64,
    ) -> Result<()> {
        self.subscribe().await?;
        let state = self
            .state(Instant::now() + Duration::from_millis(700))
            .await?;
        if !state.quiet {
            bail!("Alice is busy");
        }
        if cancelled.load(Ordering::Relaxed) || epoch.load(Ordering::SeqCst) != expected {
            bail!("speech request interrupted");
        }
        let request_id = Uuid::new_v4().to_string();
        let mut directive = Vec::new();
        push_bytes(&mut directive, 1, text.as_bytes());
        push_varint(&mut directive, 16);
        push_varint(&mut directive, 1); // append; do not interrupt native speech
        let mut payload = vec![8, 18, 24, 1]; // START_VOCALIZE, events=true
        push_bytes(&mut payload, 4, b"luna");
        push_bytes(&mut payload, 10, request_id.as_bytes());
        push_bytes(&mut payload, 13, &directive);
        self.send(8, &payload).await?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut started = false;
        let mut superseded = false;
        let result = async {
            loop {
                // New native input always wins over a package response. Do not
                // send global STOP_TTS after a newer dialog has taken ownership.
                if epoch.load(Ordering::SeqCst) != expected {
                    break Err(anyhow!("new native input interrupted speech"));
                }
                if cancelled.load(Ordering::Relaxed) {
                    break Err(anyhow!("invocation stopped"));
                }
                if Instant::now() >= deadline {
                    break Err(anyhow!("native TTS completion timed out"));
                }
                let frame = match self
                    .frame((Instant::now() + Duration::from_millis(50)).min(deadline))
                    .await
                {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(e) => break Err(e),
                };
                let frame = normalize_ipc_message(&frame)?;
                if let Some(event) = media::decode(&frame)? {
                    if event.phase == "listening" {
                        superseded = true;
                        break Err(anyhow!("native input interrupted speech"));
                    }
                }
                let Some(remoting) = nested_field(&frame, 210)? else {
                    continue;
                };
                let Some(event) = nested_field(&remoting, 18)? else {
                    continue;
                };
                let event_request = text_field(&event, 11)?.unwrap_or_default();
                if !event_request.is_empty() && event_request != request_id {
                    continue;
                }
                match varint_field(&event, 1)? {
                    Some(6) => started = true,
                    Some(1) if started => break Ok(()),
                    Some(4) => break Err(anyhow!("native TTS error")),
                    _ => (),
                }
            }
        }
        .await;
        // The method may have reached aliced even before STARTED arrives.
        // Revoke that queued TTS too, while this is still the same dialog epoch.
        if result.is_err() && !superseded && epoch.load(Ordering::SeqCst) == expected {
            let _ = self.method(14).await;
        }
        result
    }
}
fn remoting(protocol: SpeechProtocol, field: u64, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    push_bytes(&mut body, 7, b"AliceCapability");
    push_bytes(&mut body, field, payload);
    let mut frame = Vec::new();
    match protocol {
        SpeechProtocol::Direct => push_bytes(&mut frame, 210, &body),
        SpeechProtocol::Envelope => {
            push_bytes(&mut frame, 252, &body);
            push_varint(&mut frame, 253 << 3);
            push_varint(&mut frame, 210);
        }
    }
    frame
}
