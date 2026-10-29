#!/usr/bin/env python3
"""Host integration test: python3 tests/media_provider_http.py /path/to/luna.

Starts two simulated package players on loopback in a temporary data directory.
No device, media service or external network is used.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def request(base, path, body=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-Luna-Token"] = token
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, headers=headers)
    with urllib.request.urlopen(req, timeout=3) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def adapter():
    base, token = os.environ["LUNA_API_URL"], os.environ["LUNA_API_TOKEN"]
    # This token belongs solely to this temporary test invocation.
    Path("test-token").write_text(token)
    state = "paused"
    track = 0
    while True:
        request(base, "/provider/state", {"state": state, "metadata": {
            "id": str(track), "title": "Track " + str(track), "artists": ["Test artist"],
            "duration_ms": 120000, "artwork_url": "https://example.com/cover.jpg",
        }}, token)
        cmd = request(base, "/provider/next", {}, token)
        if cmd:
            success = not Path("reject-next").exists()
            if cmd["action"] in ("pause", "stop"):
                state = "paused"
            elif cmd["action"] == "resume":
                state = "playing"
            elif cmd["action"] == "next":
                track += 1
            elif cmd["action"] == "previous":
                track -= 1
            request(base, "/provider/state", {"state": state, "metadata": {
                "id": str(track), "title": "Track " + str(track), "artists": ["Test artist"],
            }}, token)
            request(base, "/provider/result", {
                "id": cmd["id"], "success": success, "state": state,
            }, token)
        time.sleep(0.05)


def wait_for(check):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, urllib.error.HTTPError):
            pass
        time.sleep(0.05)
    raise AssertionError("condition did not become true")


def rejected(base, path, body, token=None):
    try:
        request(base, path, body, token)
    except urllib.error.HTTPError as error:
        assert 400 <= error.code < 500
        return
    raise AssertionError("request unexpectedly accepted: " + path)


def run(binary, with_mpris=False):
    with tempfile.TemporaryDirectory(prefix="luna-media-test-") as tmp:
        root = Path(tmp)
        config = "allow_plugin_binaries = true\n"
        address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").split(",")[0]
        if with_mpris:
            assert address.startswith("unix:path="), "use dbus-run-session"
            config += "[media]\nmpris_bus = " + json.dumps(address) + "\n"
        (root / "settings.toml").write_text(config)
        for name, actions in [("radio", '["pause", "resume"]'),
                              ("player", '["pause", "resume", "stop", "next", "previous"]')]:
            package = root / "plugins" / name
            package.mkdir(parents=True)
            (package / "plugin.toml").write_text(f'''id = "{name}"
name = "{name}"
version = "1"
entry = "run.sh"
permissions = ["exec", "media.provider"]
[media_provider]
label = "{name}"
actions = {actions}
[launch]
kind = "binary"
mode = "service"
interpreter = "sh"
''')
            (package / "adapter.py").write_text(Path(__file__).read_text())
            (package / "run.sh").write_text('exec python3 adapter.py --adapter\n')
        with socket.socket() as port_socket:
            port_socket.bind(("127.0.0.1", 0))
            port = port_socket.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        with (root / "server.log").open("w") as log:
            server = subprocess.Popen([str(Path(binary).resolve()), "--listen", f"127.0.0.1:{port}",
                                       "--data-dir", str(root)], stdout=log, stderr=log)
            try:
                wait_for(lambda: request(base, "/api/media"))
                for name in ["radio", "player"]:
                    request(base, f"/api/plugins/{name}/toggle", {})
                    request(base, f"/api/plugins/{name}/service/start", {})
                wait_for(lambda: len(request(base, "/api/media")["providers"]) == 2)
                rejected(base, "/api/runtime/radio/provider/state", {"state": "playing"})
                player_token = (root / "plugins/player/test-token").read_text()
                rejected(base, "/api/runtime/radio/provider/state", {"state": "playing"}, player_token)
                request(base, "/api/media/select", {"provider": "radio"})
                rejected(base, "/api/media/command", {"action": "next"})
                request(base, "/api/media/command", {"action": "resume"})
                wait_for(lambda: any(p["id"] == "radio" and p["state"] == "playing" for p in request(base, "/api/media")["providers"]))
                switch = request(base, "/api/media/select", {"provider": "player"})
                wait_for(lambda: request(base, "/api/media")["selected"] == "player")
                status = request(base, "/api/media")
                assert all(p["state"] == "paused" for p in status["providers"])
                assert status["last_result"]["id"] == switch["id"]
                if with_mpris:
                    def bus(*args, check=True):
                        result = subprocess.run(["busctl", "--address=" + address, "--timeout=8", *args],
                                                text=True, capture_output=True, timeout=10)
                        if check:
                            assert result.returncode == 0, result.stderr
                        return result
                    def call(method, check=True):
                        return bus("call", "org.mpris.MediaPlayer2.luna", "/org/mpris/MediaPlayer2",
                                   "org.mpris.MediaPlayer2.Player", method, check=check)
                    def prop(name):
                        return bus("get-property", "org.mpris.MediaPlayer2.luna", "/org/mpris/MediaPlayer2",
                                   "org.mpris.MediaPlayer2.Player", name).stdout
                    wait_for(lambda: request(base, "/api/media")["mpris_connected"])
                    call("Play")
                    assert '"Playing"' in prop("PlaybackStatus")
                    call("PlayPause")
                    assert '"Paused"' in prop("PlaybackStatus")
                    before = prop("Metadata")
                    assert "Test artist" in before and "Track 0" in before
                    call("Next")
                    assert "Track 1" in prop("Metadata")
                    call("Previous")
                    assert "Track 0" in prop("Metadata")
                    call("Stop")
                    assert '"Paused"' in prop("PlaybackStatus")
                    marker = root / "plugins/player/reject-next"
                    marker.touch()
                    assert call("Next", check=False).returncode != 0, "MPRIS returned success for a rejected action"
                    marker.unlink()
                    wait_for(lambda: any(p["id"] == "player" and p["state"] == "paused" for p in request(base, "/api/media")["providers"]))
                    print("PASS: real D-Bus MPRIS controls, confirmed replies, track metadata, provider errors")
                request(base, "/api/media/command", {"action": "next"})
                wait_for(lambda: request(base, "/api/media")["pending"] is None)
                request(base, "/api/plugins/player/stop", {})
                rejected(base, "/api/runtime/player/provider/state", {"state": "paused"}, player_token)
                rejected(base, "/api/media/command", {"action": "resume"})
                assert request(base, "/api/media")["selected"] is None
                print("PASS: two adapters, handoff, capabilities, scoped tokens, revoked tokens, stopped owner")
            except Exception:
                print((root / "server.log").read_text()[-4000:])
                raise
            finally:
                server.send_signal(signal.SIGTERM)
                try:
                    server.wait(timeout=25)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
                    raise AssertionError("Luna did not shut down")


if __name__ == "__main__":
    adapter() if sys.argv[1] == "--adapter" else run(sys.argv[1], "--mpris" in sys.argv[2:])
