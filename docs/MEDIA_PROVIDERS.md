# Media providers

Luna can select a package-owned player and deliver transport commands to its
current invocation. The package handles its player protocol, authentication,
queue and playback confirmation. This API has no dependency on Spotify.

This coordinates registered adapters only. It does not intercept Alice speech
or control the stock music player. A stock-player adapter and a voice router
must be integrated separately before claiming exclusive control of all music.
Weather, timers and other Alice functions are not modified by this API.

## Declare an adapter

A local binary package declares its supported actions in `plugin.toml`:

```toml
permissions = ["exec", "media.provider"]

[media_provider]
label = "Internet radio"
actions = ["pause", "resume"]
```

`pause` is required. `resume`, `next` and `previous` are optional. The label is
1–80 UTF-8 bytes without control characters. Existing packages without this
section remain unchanged. This version uses the binary runtime API; it does
not add Lua bindings. Binary packages imported from YAM use the same invocation
contract when binary execution is explicitly enabled; YAM owns the payload and
Luna owns the process. Do not start the same player as a second YAM service.

## Runtime protocol

Use the invocation's `LUNA_API_URL` as the base URL and `LUNA_API_TOKEN` in the
`X-Luna-Token` header. These variables are provided by Luna, not saved in the
manifest. A token cannot control another package's provider.

| Request relative to `LUNA_API_URL` | Body / response |
| --- | --- |
| `POST /provider/state` | `{"state":"paused"}`; returns 204 |
| `POST /provider/next` | Empty body; returns `null` or a command |
| `POST /provider/result` | `{"id":"…","success":true,"state":"paused"}`; returns 204 |

States are `unknown`, `paused`, `playing`. Report actual observed state at least
once a second; after five seconds without a report the provider is unavailable.
Do not report `paused` just because a pause request was sent.

Poll `/provider/next` regularly (for example, every 100–250 ms). A command is:

```json
{"id":"uuid","action":"pause","expires_in_ms":4900}
```

Execute it once, within the remaining time. Return its ID and the observed
result. A successful `pause` must confirm `paused`, and a successful `resume`
must confirm `playing`. For `next`/`previous`, the adapter must confirm the
track change or restart itself; Luna cannot infer it from playback state.

A command is delivered only once. A lost poll response or missing result
expires after five seconds and leaves the outcome unknown. Luna never retries
it automatically. Reconcile with the player and report state before proceeding.
Old invocation tokens, duplicate results, undelivered IDs and late results are
rejected. A failed result leaves playback state unknown.

## Selection and transport

- `GET /api/media` lists provider capabilities, availability and observed state,
  plus `selected`, `pending` and `last_result`.
- `POST /api/media/select` with `{"provider":"package-id"}` selects a provider.
- `POST /api/media/command` with `{"action":"resume"}` addresses the selected provider.

Mutations return HTTP 202 with an operation `id`. This means accepted, not
completed. Observe `pending` and `last_result` through `GET /api/media`.
Only one operation may be pending. The most recent result is retained; this
is not a persistent operation log. Disconnecting the HTTP caller does not
cancel a pending operation.

The target must be available and paused. If the current provider is playing,
Luna asks it to pause and changes selection only after confirmation. The target
must still belong to the same invocation and remain available and paused.
Unknown state, timeout or failure leaves the existing selection intact. Selection
does not start playback. Pausing does not clear selection.

If the selected package stops, it remains selected but unavailable; commands
fail instead of starting another source. Luna starts with no selection after a
restart. State is not persisted and there is no automatic fallback or resume.
An adapter must not start an unselected player independently if the integration
requires exclusive ownership; Luna does not police external phones or stock Alice.

## Optional indication

Use `GET /api/clock` to check `text_available`, then `POST /api/clock/text` for a
short label or scrolling text. This is independent of provider selection and
has a narrower character set than provider labels. Do not pass an arbitrary
label without validating it against the display API. See [API.md](API.md).

## Verification

```sh
cargo test --workspace --locked
cargo build --bin luna --locked
python3 tests/media_provider_http.py target/debug/luna
```

The HTTP test runs two simulated binary adapters on loopback in a temporary
data directory. It checks handoff without starting the target, differing
capabilities and invocation-scoped tokens. It does not test speaker audio,
voice recognition or firmware-specific native playback.
