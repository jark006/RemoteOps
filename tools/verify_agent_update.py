"""Verify a real, unsupervised Agent update and Unix startup rollback.

Uses private temporary binaries and a loopback port. Does not replace an
installed Agent or modify service-manager configuration.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", type=Path, required=True)
    parser.add_argument("--proxy", type=Path, required=True)
    parser.add_argument("--systemd", action="store_true", help="Linux root: create an isolated transient test unit with the recommended policy")
    parser.add_argument("--defer-reap", action="store_true", help="Linux: leave the old Agent as a zombie until the update completes")
    args = parser.parse_args()
    source = args.agent.resolve(strict=True)
    proxy_binary = args.proxy.resolve(strict=True)
    if args.systemd and (os.name == "nt" or os.geteuid() != 0):
        parser.error("--systemd requires Linux root")
    if args.defer_reap and (not sys.platform.startswith("linux") or args.systemd):
        parser.error("--defer-reap requires unsupervised Linux")
    with tempfile.TemporaryDirectory(prefix="remote-ops-update-") as temporary:
        root = Path(temporary)
        agent_path = root / ("agent.exe" if os.name == "nt" else "agent")
        candidate = root / ("candidate.exe" if os.name == "nt" else "candidate")
        shutil.copy2(source, agent_path)
        shutil.copy2(source, candidate)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        with tempfile.TemporaryFile() as log:
            unit = f"remote-ops-update-fixture-{os.getpid()}.service" if args.systemd else None
            command = [str(agent_path), "--listen", f"127.0.0.1:{port}"]
            old = None
            if unit:
                subprocess.run(["systemd-run", "--unit", unit, "--collect", "--property=KillMode=process", "--property=Restart=on-failure", *command], check=True, capture_output=True, timeout=10)
                old_pid = int(subprocess.check_output(["systemctl", "show", "--property=MainPID", "--value", unit], text=True, timeout=5))
            else:
                old = subprocess.Popen(command, cwd=root, stdout=log, stderr=log)
                old_pid = old.pid
            # Reap the old Agent immediately after it exits, including Unix
            # zombies that kill(pid,0) would otherwise continue to report alive.
            reaper = threading.Thread(target=old.wait, daemon=True) if old and not args.defer_reap else None
            if reaper:
                reaper.start()
            proxy = subprocess.Popen([str(proxy_binary), "--remote", f"127.0.0.1:{port}"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
            current_pid = old_pid
            request_id = 0
            def call(name, arguments):
                nonlocal request_id
                request_id += 1
                request = {"jsonrpc": "2.0", "id": request_id, "method": "tools/call", "params": {"name": name, "arguments": arguments}}
                proxy.stdin.write(json.dumps(request) + "\n")
                proxy.stdin.flush()
                response = json.loads(proxy.stdout.readline())
                assert "error" not in response and not response["result"].get("isError"), response
                return response["result"]["structuredContent"]
            try:
                deadline = time.monotonic() + 10
                while True:
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            break
                    except OSError:
                        if time.monotonic() >= deadline:
                            log.seek(0)
                            raise RuntimeError(log.read().decode(errors="replace"))
                        time.sleep(0.05)
                before = call("agent_info", {"response_mode": "full"})
                success = call("agent_update", {"local_path": str(candidate), "timeout_ms": 20000, "poll_interval_ms": 100, "probe_timeout_ms": 1000})
                current_pid = success["current_agent"]["runtime"]["pid"]
                assert success["status"] == "updated", success
                assert success["update_receipt"]["status"] == "updated", success
                assert success["update_receipt"]["previous_instance_id"] == before["runtime"]["instance_id"]
                assert current_pid != old_pid
                assert hashlib.sha256(agent_path.read_bytes()).hexdigest() == hashlib.sha256(source.read_bytes()).hexdigest()
                cases = ["updated-identical-binary"]
                if args.defer_reap:
                    state = Path(f"/proc/{old_pid}/stat").read_text().rsplit(") ", 1)[1].split()[0]
                    assert state == "Z", state
                    old.wait(timeout=5)
                    cases.append("updated-with-unreaped-parent")
                if os.name != "nt":
                    check = json.loads(subprocess.check_output([str(source), "--self-check"], text=True))
                    script = "#!/bin/sh\nif [ \"$1\" = --self-check ]; then\ncat <<'REMOTE_OPS_FIXTURE'\n" + json.dumps(check) + "\nREMOTE_OPS_FIXTURE\nexit 0\nfi\nexit 37\n"
                    candidate.write_text(script, encoding="utf-8")
                    candidate.chmod(0o755)
                    rollback = call("agent_update", {"local_path": str(candidate), "timeout_ms": 20000, "poll_interval_ms": 100, "probe_timeout_ms": 1000})
                    current_pid = rollback["current_agent"]["runtime"]["pid"]
                    assert rollback["status"] == "rolled_back", rollback
                    assert rollback["update_receipt"]["status"] == "rolled_back", rollback
                    assert hashlib.sha256(agent_path.read_bytes()).hexdigest() == hashlib.sha256(source.read_bytes()).hexdigest()
                    cases.append("startup-failure-rolled-back")
                assert not list(root.glob("*.rollback-*")), list(root.iterdir())
                if unit:
                    assert subprocess.check_output(["systemctl", "is-active", unit], text=True, timeout=5).strip() == "active"
                    supervised_pid = int(subprocess.check_output(["systemctl", "show", "--property=MainPID", "--value", unit], text=True, timeout=5))
                    assert supervised_pid == current_pid
                print(json.dumps({"ok": True, "cases": cases, "receipt": "hash+instance+attempt", "systemd": bool(unit)}))
            finally:
                proxy.stdin.close()
                try:
                    proxy.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proxy.kill()
                    proxy.wait(timeout=5)
                if unit:
                    subprocess.run(["systemctl", "stop", unit], check=True, capture_output=True, timeout=10)
                else:
                    try:
                        os.kill(current_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                if reaper:
                    reaper.join(timeout=5)
                elif old:
                    try:
                        old.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        old.kill()
                        old.wait(timeout=5)
                # Give the helper's short-lived cleanup process time to exit.
                time.sleep(0.3)


if __name__ == "__main__":
    main()
