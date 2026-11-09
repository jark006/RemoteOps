"""Exercise a static Linux Agent in a BusyBox-only root, with no systemd.

Run as root on Linux. Inputs are ordinary test artifacts; the fixture creates
and removes its own temporary root and never installs a system service.
"""

import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True, type=Path)
    parser.add_argument("--proxy", required=True, type=Path)
    parser.add_argument("--busybox", required=True, type=Path)
    args = parser.parse_args()
    if os.geteuid() != 0:
        parser.error("the temporary chroot fixture requires root")
    artifacts = {name: path.resolve(strict=True) for name, path in vars(args).items()}
    with tempfile.TemporaryDirectory(prefix="remote-ops-embedded-") as temporary, ExitStack() as cleanup:
        root = Path(temporary)
        for directory in ["bin", "dev", "tmp", "work"]:
            (root / directory).mkdir()
        shutil.copyfile(artifacts["agent"], root / "agent")
        shutil.copyfile(artifacts["busybox"], root / "bin/busybox")
        (root / "agent").chmod(0o755)
        (root / "bin/busybox").chmod(0o755)
        (root / "bin/sh").symlink_to("busybox")
        os.mknod(root / "dev/null", stat.S_IFCHR | 0o666, os.makedev(1, 3))
        # /tmp may be nodev (including on WSL). Bind the real device rather than
        # assuming a newly created device inode is usable on this filesystem.
        subprocess.run(["mount", "--bind", "/dev/null", str(root / "dev/null")],check=True,timeout=5)
        cleanup.callback(lambda: subprocess.run(["umount",str(root / "dev/null")],check=True,timeout=5))
        preflight = subprocess.run(
            [shutil.which("chroot"), str(root), "/bin/busybox", "sh", "-c", "/bin/busybox cat /dev/null; printf preflight"],
            capture_output=True, text=True, timeout=5, check=True,
        )
        assert preflight.stdout == "preflight", preflight
        assert not preflight.stderr, preflight.stderr
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        with tempfile.TemporaryFile() as log:
            agent = subprocess.Popen(
                [shutil.which("chroot"), str(root), "/agent", "--listen", f"127.0.0.1:{port}"],
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, env={"PATH": "/bin"},
            )
            try:
                deadline = time.monotonic() + 10
                while True:
                    if agent.poll() is not None:
                        log.seek(0)
                        raise RuntimeError(log.read().decode(errors="replace"))
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                            break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(0.05)
                calls = [
                    ("environment", {}),
                    ("sh_exec", {"shell": "busybox", "command": 'printf "%s" "$REMOTE_OPS_FIXTURE"; pwd', "cwd": "/work", "env": {"REMOTE_OPS_FIXTURE": "fixture:"}}),
                    ("sh_exec", {"command": "printf native; exit 7"}),
                    ("sh_exec", {"shell": "busybox", "command": "# " + "x" * 140_000 + "\nprintf long-script"}),
                ]
                messages = [
                    {"jsonrpc": "2.0", "id": index, "method": "tools/call", "params": {"name": name, "arguments": arguments}}
                    for index, (name, arguments) in enumerate(calls)
                ]
                proxy = subprocess.run(
                    [str(artifacts["proxy"]), "--remote", f"127.0.0.1:{port}"],
                    input="".join(json.dumps(message) + "\n" for message in messages),
                    text=True, capture_output=True, timeout=30, check=True,
                )
                assert not proxy.stderr, proxy.stderr
                responses = [json.loads(line) for line in proxy.stdout.splitlines()]
                assert len(responses) == len(messages)
                if any(response.get("result", {}).get("isError") for response in responses):
                    log.seek(0)
                    raise RuntimeError(json.dumps(responses, ensure_ascii=False) + "\n" + log.read().decode(errors="replace"))
                for response in responses:
                    assert response["result"]["isError"] is False, response
                profile, explicit, native, long = [response["result"]["structuredContent"] for response in responses]
                shells = {item["name"] for item in profile["shells"]["items"]}
                assert shells == {"sh", "busybox"}, profile
                assert profile["shells"]["default"] == "sh"
                assert profile["init_system"]["name"] is None, profile
                assert {item["name"] for item in profile["utilities"]["items"]} == {"busybox"}
                assert explicit["stdout"] == "fixture:/work\n", explicit
                assert explicit["shell"]["name"] == "busybox"
                assert native["stdout"] == "native" and native["exit_code"] == 7, native
                assert native["shell"]["name"] == "sh"
                assert long["stdout"] == "long-script" and long["exit_code"] == 0, long
                print(json.dumps({"ok": True, "fixture": "busybox-only chroot", "shells": sorted(shells), "systemctl_detected": False, "pid1_observed": False, "verified_requests": len(calls)}))
            finally:
                agent.terminate()
                try:
                    agent.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    agent.kill()
                    agent.wait(timeout=5)


if __name__ == "__main__":
    main()
