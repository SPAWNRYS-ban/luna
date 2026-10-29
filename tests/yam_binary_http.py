#!/usr/bin/env python3
"""Real runtime lifecycle for a read-only, symlinked YAM binary package."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def call(base, path, body=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["X-Luna-Token"] = token
    data = json.dumps(body).encode() if body is not None else None
    with urllib.request.urlopen(urllib.request.Request(base + path, data=data, headers=headers), timeout=5) as reply:
        raw = reply.read()
        return json.loads(raw) if raw else None


def wait(check):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, urllib.error.HTTPError):
            pass
        time.sleep(.05)
    raise AssertionError("fixture did not become ready")


with tempfile.TemporaryDirectory(prefix="luna-yam-binary-") as temporary:
    root = Path(temporary)
    yam = root / "yam"
    payload = yam / "pkgs/local/provider/1.0.0/plugin"
    shared = yam / "share/luna/plugins/local.provider"
    data = root / "data"
    payload.mkdir(parents=True)
    shared.mkdir(parents=True)
    data.mkdir()
    (payload / "plugin.toml").write_text('''id="local.provider"
name="YAM fixture"
version="1.0.0"
entry="run.sh"
permissions=["exec", "media.provider"]
[launch]
kind="binary"
mode="service"
path="run.sh"
interpreter="sh"
[media_provider]
label="YAM fixture"
actions=["pause", "resume"]
''')
    (payload / "run.sh").write_text('''set -eu
test "$YAM_ROOT" = "$EXPECTED_YAM"
test "$YAM_VAR" = "$YAM_ROOT/var"
umask 077
printf %s "$LUNA_API_TOKEN" > "$LUNA_PLUGIN_STATE/token"
trap 'exit 0' TERM
while :; do sleep 0.1; done
''')
    for path in payload.iterdir():
        path.chmod(0o444)
        (shared / path.name).symlink_to(path)
    before = {p.name: hashlib.sha256(p.read_bytes()).digest() for p in payload.iterdir()}
    (data / "settings.toml").write_text("allow_plugin_binaries=true\n")
    plugins = data / "plugins"
    plugins.mkdir()
    (plugins / "enabled.json").write_text('["local.provider"]')
    (plugins / "services.json").write_text('["local.provider"]')
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    with (root / "runtime.log").open("w") as log:
        daemon = subprocess.Popen([str(Path(sys.argv[1]).resolve()), "--listen", f"127.0.0.1:{port}",
                                   "--data-dir", str(data), "--yam-root", str(yam)],
                                  env=dict(os.environ, EXPECTED_YAM=str(yam)), stdout=log, stderr=log)
        try:
            token_file = plugins / "plugin-state/local.provider/token"
            wait(token_file.exists)
            token = token_file.read_text()
            listing = call(base, "/api/plugins")
            assert len(listing) == 1 and listing[0]["source"] == "yam"
            assert listing[0]["compatible"] and listing[0]["running"]
            assert not call(base, "/api/settings")["updates"]["enabled"]
            path = "/api/runtime/local.provider/provider/state"
            call(base, path, {"state": "paused"}, token)
            call(base, "/api/media/select", {"provider": "local.provider"})
            assert call(base, "/api/media")["selected"] == "local.provider"
            call(base, "/api/plugins/local.provider/stop", {})
            try:
                call(base, path, {"state": "paused"}, token)
            except urllib.error.HTTPError:
                pass
            else:
                raise AssertionError("stopped package retained runtime token")
            after = {p.name: hashlib.sha256(p.read_bytes()).digest() for p in payload.iterdir()}
            assert before == after and len(list(payload.iterdir())) == 2
            print("PASS: YAM binary startup, namespace, provider, environment, read-only payload, token revocation, updates disabled")
        except Exception:
            log.flush()
            print((root / "runtime.log").read_text()[-4000:])
            raise
        finally:
            daemon.terminate()
            daemon.wait(timeout=25)
