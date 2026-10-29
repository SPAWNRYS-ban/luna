#!/usr/bin/env python3
"""Exercise the BusyBox service helper with an empty runtime directory."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import urllib.request

binary = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix="luna-service-") as temporary:
    root = Path(temporary)
    yam = root / "yam"
    (yam / "bin").mkdir(parents=True)
    (yam / "bin/luna").symlink_to(binary)
    helper = yam / "bin/luna-service"
    shutil.copy2(Path(__file__).resolve().parents[1] / "tools/luna-service", helper)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    run = root / "service-run"
    run.mkdir()
    # An old pidfile must never make the helper signal an unrelated process.
    other = subprocess.Popen(["sleep", "60"])
    (run / "pid").write_text(str(other.pid))
    env = dict(os.environ, YAM_ROOT=str(yam), YAM_RUN=str(root / "new-yam-run"),
               LUNA_RUN=str(run), LUNA_LISTEN=f"127.0.0.1:{port}")

    def service(action, check=True):
        result = subprocess.run([str(helper), action], env=env, check=False,
                                capture_output=True, text=True, timeout=30)
        if check and result.returncode:
            raise AssertionError(f"{action} failed: {result.stdout}{result.stderr}")
        return result

    try:
        assert service("status", check=False).returncode != 0
        service("start")
        service("status")
        assert (root / "new-yam-run/yam.lock").exists()
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/settings", timeout=3) as reply:
            assert not json.load(reply)["updates"]["enabled"]
        before = (run / "pid").read_text()
        service("start")
        assert before == (run / "pid").read_text(), "duplicate daemon"
        service("stop")
        assert service("status", check=False).returncode != 0
        lock = root / "new-yam-run/yam.lock"
        lock.write_bytes(b"existing-lock")
        service("start")
        assert lock.read_bytes() == b"existing-lock", "existing lock was truncated"
        service("stop")
        assert other.poll() is None, "stale pidfile killed an unrelated process"
        print("PASS: fresh boot directory, readiness, idempotent start, stop, stale PID safety")
    finally:
        service("stop", check=False)
        other.terminate()
        other.wait(timeout=3)
