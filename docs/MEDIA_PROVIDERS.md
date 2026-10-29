# Media providers (runtime API v2)

Luna coordinates package-owned players. Packages retain their catalogue,
authentication, queue, decoding and confirmation of actual playback. Luna owns
selection, shared transport, the optional MPRIS bridge, voice claims and
transient audio focus. This contract is not specific to YouTube or Spotify.

## Package and invocation

A binary package declares its capabilities in `plugin.toml`:

```toml
permissions = ["exec", "media.provider", "ipc.speech", "commands", "speech.tts"]

[media_provider]
label = "Music"
actions = ["pause", "resume", "stop", "next", "previous"]
ducking = true

[commands]
phrases = ["мои любимые треки"]
patterns = ["включи на ютубе {query}"]
exclude = ["включи на ютубе таймер {query}"]

[launch]
kind = "binary"
mode = "service"
path = "bin/player-adapter"
```

Only `pause` is required; declare other actions and `ducking` only when the
adapter implements them. Permissions are independent: a provider need not read
speech or synthesize it. `media.focus` additionally allows temporary focus leases.
New APIs currently have binary HTTP bindings; existing Lua bindings are unchanged.

Use `LUNA_API_URL` and the invocation's `LUNA_API_TOKEN` as `X-Luna-Token`.
Do not persist or log the token. `GET /media` relative to the runtime URL reports
API `version: 2`, permitted features, native speech connectivity and actual MPRIS
connectivity. The equivalent global provider status is `GET /api/media`.

YAM owns installed files; Luna owns the adapter process and token. See
[PLUGINS.md](PLUGINS.md). Never start the same player under both supervisors.
Write mutable data to `LUNA_PLUGIN_STATE` or YAM var, not the package tree.

## Report observed state

`POST /provider/state` returns 204:

```json
{
  "state": "playing",
  "metadata": {
    "id": "track-id",
    "title": "Track",
    "artists": ["Artist"],
    "duration_ms": 180000,
    "artwork_url": "https://example.org/cover.jpg"
  },
  "position_ms": 1200,
  "applied_gain": 1.0
}
```

States are `unknown`, `paused`, `playing`. Report at least once a second; five
seconds without a heartbeat makes the provider unavailable. Metadata, position
and applied gain are optional and survive a heartbeat that omits them. Send
`clear_metadata: true` to remove a track; do not include `metadata` in that request.
A new invocation starts without old metadata. Artwork is a URL, never fetched by
Luna. Text, duration, position, gain and URL bounds are validated.

Do not report paused merely because a pause command was sent. Unknown outcomes
must remain unknown until the real player reconciles them.

## Selection and transport

- `GET /api/media`: selected ID, providers, focus, pending operation, last result.
- `POST /api/media/select` with `{"provider":"package-id"}`: select a paused provider.
- `POST /api/media/command` with `{"action":"resume"}`: control the selected provider.

Mutations return 202 and `{"id":"operation-id"}`. This is acceptance, not
completion. Observe `pending` and `last_result` through GET. There is one pending
operation and no automatic retry. Selection never starts playback.

If the current provider is playing, Luna requests pause and changes selection
only after a successful, invocation-scoped acknowledgement. Unknown state,
stale heartbeat, failure or target restart leaves selection unchanged.

Poll `POST /provider/next` every 100–250 ms. It returns `null` or:

```json
{"id":"operation-id","action":"pause","expires_in_ms":4900}
```

Execute once within the remaining time. Report the ID, success and observed
state to `POST /provider/result`:

```json
{"id":"operation-id","success":true,"state":"paused"}
```

Pause and stop require paused confirmation; resume requires playing. Next and
previous must confirm a track change or restart in the adapter, not merely an
unchanged playing status. Late, duplicate, undelivered or wrong-invocation
results are rejected. A timeout or failure does not grant another source output.

An unavailable provider remains selected until output release is known. Only
confirmed process-group cleanup clears selection. A lost heartbeat or revoked
token alone does not prove that the player stopped. Failed cleanup blocks a
replacement invocation. There is no automatic fallback or resume after restart.

## Shared controls and the physical button

The MPRIS bridge is disabled by default. To use an existing local player bus,
add to Luna's `settings.toml`:

```toml
[media]
mpris_bus = "unix:path=/tmp/btbus/socket"
voice_controls = true
```

Luna exports one `org.mpris.MediaPlayer2.luna` object. Play, Pause, PlayPause,
Stop, Next and Previous address the selected provider and wait for its result;
metadata and capabilities come from provider reports. Unsupported operations
fail. No seeking or URI opening is advertised. Volume remains with native
maind/ALSA; the bridge does not apply a second master gain.

The existing bt-proxy can expose this object to native controls, including the
physical pause button. Luna does not install bt-proxy, reconfigure Bluetooth or
create its bus. Disable an adapter's own MPRIS export when Luna owns this bridge,
otherwise the same player appears twice. Reconnection does not resume music.

With `voice_controls = true`, a supported native backend and a selected provider,
Luna also handles exact Russian transport phrases: «пауза», «поставь на паузу»,
«продолжи музыку», «останови музыку», «выключи музыку», «следующий трек»,
«предыдущий трек», «что сейчас играет» and their declared aliases in the router.
These go to the same transport broker. Track announcements use reported metadata
and native TTS. Missing providers/capabilities leave the request to native Alice.
Volume, weather, alarms and other requests retain their normal handling.

## Package voice commands

Luna keeps one persistent aliced subscription, independent of package polling.
It exposes only request text, phase and IDs, not private response cards or raw IPC.

`POST /commands/next` requires both `commands` and `ipc.speech`. Poll repeatedly
with a client timeout of at least six seconds. It returns `null` or a claimed,
confirmed-cancelled native request:

```json
{
  "id":"command-receipt",
  "request_id":"native-request-id",
  "transcript":"включи на ютубе jazz",
  "pattern":"включи на ютубе {query}",
  "query":"jazz"
}
```

Only final ASR results marked as recognition started, with a fresh request ID, can be claimed (1.5-second claim
window). Startup/reconnection snapshots, partials, closed dialogs and duplicate
claims cannot execute a command. A request has one global owner. Shared transport
wins when enabled; otherwise the matching selected provider wins, then an exact
phrase, then the longest pattern prefix, then package ID. Exclusions apply before
matching. `{query}` is allowed only at the end and must capture a nonempty tail.
Normalization ignores case, punctuation and a leading «Алиса».

For observational use, `POST /speech/next` returns bounded events with `phase`,
`request_id`, `transcript`, `final`, `initial_snapshot`, `source`, `event_id`.
It does not cancel or claim anything. Legacy `POST /command/cancel` with
`{"request_id":"…"}` uses the same global policy and claim checks. Do not poll
`speech/next` and `commands/next` concurrently for one invocation: they share its
cursor and reject simultaneous polls. A new subscriber receives future events,
not a replay. Local spotter events are observational only;
the native MPRIS action must not be executed a second time.

Native cancellation uses a fixed, model-specific backend from `StationSpec`.
The first Lite uses the 252/253 envelope and STOP_CONVERSATION. Mini 1 uses its
independent direct remoting sequence. Other station crates advertise no native
speech control until a backend is verified; they can still use provider APIs.
Do not copy one firmware's control sequence to another model.

## Native speech synthesis

`POST /speech/say` requires `speech.tts`:

```json
{"text":"Включаю","command_id":"command-receipt"}
```

`command_id` is optional. When supplied, it must belong to this invocation, be
less than 30 seconds old and precede no newer voice request. Text is limited to
1–1000 characters without control characters. Use an HTTP timeout of 20 seconds.
The response `{"completed":true}` means a native TTS STARTED and then COMPLETED
were observed, not just that a message was sent. Native synthesis can require the
Station's existing network services; this API is not an offline synthesizer.

Speech operations are serialized with no unbounded queue. An unrelated active
native dialog is never cancelled merely to speak a package message. A new voice
request interrupts the package's speech. Owner stop, native error or timeout
releases focus and stops its pending TTS while the dialog still belongs to it.
An HTTP disconnect does not prematurely drop focus or repeat the operation.
Do not retry an uncertain command or utterance automatically.

## Transient audio focus

A provider declaring `ducking = true` polls `GET /focus` relative to its runtime
URL every 100–250 ms. Apply its `gain` (1.0 or 0.2) in that player's audio path and
report `applied_gain`. This must not change ALSA Master. Unselected providers get
1.0. Restore 1.0 on API failure within a short bounded interval; never persist a
transient gain. Native listening, thinking, speaking and package TTS duck the
selected provider. A lost native connection restores focus; missing idle is also
bounded by a 30-second watchdog. TTS allows up to 300 ms for gain acknowledgement.

Other packages may request a finite lease with permission `media.focus`:

```text
POST /focus/acquire  {"ttl_ms":1000}  -> {"lease":"lease-id"}
POST /focus/release  {"lease":"lease-id"} -> 204
```

TTL is 100–30000 ms, at most 32 leases. Leases are invocation-owned, nested and
expire automatically. Releasing one owner cannot cancel another owner's focus.
Stopping an owner revokes its leases; playback is never automatically resumed.

## Verification and hardware status

```sh
cargo test --workspace --locked
cargo build --bin luna --locked
python3 tests/media_provider_http.py target/debug/luna
python3 tests/yam_binary_http.py target/debug/luna
python3 tests/yam_service.py target/debug/luna
# Linux with dbus-daemon and busctl:
dbus-run-session -- python3 tests/media_provider_http.py target/debug/luna --mpris
```

Tests use real loopback HTTP/TCP and D-Bus with simulated native endpoints and
players. They check handoff, scoped tokens, replay/claim races, both wire formats,
TTS completion/error/revocation, nested focus, read-only YAM files and lifecycle.
They do not prove acoustic playback or wake-word recovery on hardware. This
experimental branch still needs an end-to-end Station test with an adapted
player; earlier physical testing was limited to the first Lite.

Protocol references: [Alice state](https://github.com/v3rt3p/quasar-protos/blob/master/yandex_io/protos/alice_state.proto),
[fixed remoting methods](https://github.com/v3rt3p/quasar-protos/blob/master/yandex_io/protos/remoting.proto),
[MPRIS specification](https://specifications.freedesktop.org/mpris/latest/Player_Interface.html).
