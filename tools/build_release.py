#!/usr/bin/env python3
"""Build an atomic, verifiable release using only Cargo-reported executables.

Python 3.9+; no third-party dependencies. Cross builds require cargo-zigbuild,
Zig, installed Rust targets, and Nightly/rust-src for the custom MIPS target.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import uuid
from datetime import datetime, timezone

ROOT = Path(__file__).resolve().parents[1]
MARKER = b"REMOTE_OPS_ARTIFACT_V1:"
MARKER_END = b":REMOTE_OPS_ARTIFACT_END"
BINS = ["remote-ops-agent", "remote-ops-proxy"]


class BuildError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise BuildError(message)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_manifest(root):
    """Hash explicit source roots, including untracked new implementation files."""
    files = []
    for name in ["Cargo.toml", "Cargo.lock", ".gitignore", ".cargo", "rust-toolchain.toml", "rust-toolchain", "crates", "targets", "tools", "deploy",
                 "docs", "README.md", "AGENTS.md", "buildAll.ps1", "LICENSE"]:
        start = root / name
        require(start.exists() or not start.is_symlink(), f"dangling source symlink: {start}")
        if not start.exists():
            continue
        pending = [start]
        while pending:
            path = pending.pop()
            require(not path.is_symlink(), f"source symlink is not allowed: {path}")
            if path.is_dir():
                pending.extend(child for child in path.iterdir() if child.name != "__pycache__")
            else:
                require(path.is_file(), f"source is not a regular file: {path}")
                files.append({"path": path.relative_to(root).as_posix(),
                              "size": path.stat().st_size, "sha256": sha256_file(path)})
    files.sort(key=lambda entry: entry["path"])
    encoded = json.dumps(files, ensure_ascii=True, separators=(",", ":")).encode()
    return {"sha256": hashlib.sha256(encoded).hexdigest(), "files": files}


def checked_output(command, root):
    try:
        return subprocess.check_output(command, cwd=root, text=True, encoding="utf-8")
    except (OSError, subprocess.CalledProcessError) as error:
        raise BuildError(f"command failed: {command}: {error}") from error


def cargo_command(target, toolchain, native):
    cargo = ["cargo"] + ([f"+{toolchain}"] if toolchain else [])
    if "spec" in target:
        return ["cargo", "+nightly", "build", "-Z", "json-target-spec", "-Z",
                "build-std=std,panic_abort", "--target", target["spec"]]
    return cargo + ["build" if native else "zigbuild", "--target", target["target"]]


def compile_target(root, target, toolchain, native, env):
    bins = target.get("bins", BINS)
    command = cargo_command(target, toolchain, native)
    command += ["--locked", "--release", "--message-format=json-render-diagnostics"]
    for name in bins:
        command += ["-p", name]
    print("Building " + target["target"], flush=True)
    artifacts = {}
    try:
        process = subprocess.Popen(command, cwd=root, env=env, stdout=subprocess.PIPE,
                                   text=True, encoding="utf-8", errors="strict")
        with process.stdout:
            for line in process.stdout:
                try:
                    message = json.loads(line)
                except json.JSONDecodeError:
                    print(line.rstrip(), file=sys.stderr)
                    continue
                if message.get("reason") == "compiler-message":
                    rendered = message.get("message", {}).get("rendered")
                    if rendered:
                        print(rendered, end="", file=sys.stderr)
                if message.get("reason") == "compiler-artifact":
                    description = message.get("target", {})
                    name = description.get("name")
                    if name in bins and "bin" in description.get("kind", []):
                        executable = message.get("executable")
                        if executable:
                            artifacts[name] = Path(executable)
        status = process.wait()
    except (OSError, UnicodeError) as error:
        if "process" in locals() and process.poll() is None:
            process.kill()
            process.wait()
        raise BuildError(f"build failed for {target['target']}: {error}") from error
    require(status == 0, f"build failed for {target['target']} (exit {status})")
    require(set(artifacts) == set(bins), f"Cargo did not report every executable for {target['target']}")
    return artifacts


def read_metadata(data, expected):
    position = 0
    found = []
    while True:
        start = data.find(MARKER, position)
        if start < 0:
            break
        start += len(MARKER)
        end = data.find(MARKER_END, start, start + 2048)
        require(end >= 0, "invalid embedded artifact metadata")
        try:
            found.append(json.loads(data[start:end]))
        except (ValueError, UnicodeError) as error:
            raise BuildError("invalid embedded artifact JSON") from error
        position = end + len(MARKER_END)
    require(bool(found), "missing embedded artifact metadata (old binary?)")
    require(all(item == expected for item in found), "artifact build provenance does not match this build")
    return found[0]


def check_binary(data, target):
    """Check architecture and static/float ABI without executing cross binaries."""
    def unpack(fmt, offset):
        require(offset >= 0 and offset + struct.calcsize(fmt) <= len(data), "truncated executable")
        return struct.unpack_from(fmt, data, offset)

    kind = target["format"]
    if kind == "pe":
        require(data[:2] == b"MZ", "expected PE executable")
        header, = unpack("<I", 0x3c)
        require(data[header:header + 4] == b"PE\0\0", "invalid PE signature")
        require(unpack("<H", header + 4)[0] == target["machine"], "wrong PE machine")
        require(unpack("<H", header + 24)[0] == 0x20b, "expected PE32+")
    elif kind == "macho":
        require(data[:4] == b"\xcf\xfa\xed\xfe", "expected little-endian Mach-O 64")
        require(unpack("<I", 4)[0] == target["machine"], "wrong Mach-O CPU")
        require(unpack("<I", 12)[0] == 2, "expected Mach-O executable")
    elif kind == "elf":
        require(data[:4] == b"\x7fELF" and len(data) >= 52, "expected ELF executable")
        bits = target["bits"]
        require(data[4] == (1 if bits == 32 else 2) and data[5] == 1,
                "wrong ELF class or byte order")
        require(unpack("<H", 16)[0] in (2, 3), "expected ELF executable or PIE")
        require(unpack("<H", 18)[0] == target["machine"], "wrong ELF machine")
        if bits == 32:
            phoff, flags = unpack("<I", 28)[0], unpack("<I", 36)[0]
            phsize, count = unpack("<HH", 42)
            minimum = 32
        else:
            phoff, flags = unpack("<Q", 32)[0], unpack("<I", 48)[0]
            phsize, count = unpack("<HH", 54)
            minimum = 56
        require(count > 0 and phsize >= minimum and phoff + phsize * count <= len(data),
                "invalid ELF program headers")
        mips_soft = False
        for index in range(count):
            offset = phoff + phsize * index
            segment, = unpack("<I", offset)
            require(segment != 3, "musl release must be static (PT_INTERP found)")
            if target["machine"] == 8 and segment == 0x70000003:
                abi_offset, = unpack("<I", offset + 4)
                require(abi_offset + 24 <= len(data), "truncated MIPS ABI flags")
                mips_soft = data[abi_offset + 7] == 3
        if target.get("float") == "hard":
            require(flags & 0x600 == 0x400, "expected ARM hard-float ABI")
        if target.get("float") == "soft":
            require(flags & 0x600 == 0x200, "expected ARM soft-float ABI")
        if target["machine"] == 8:
            require(flags & 0xf000 == 0x1000 and flags & 0xf0000000 == 0x70000000,
                    "expected MIPS32r2/o32 ABI")
            require(mips_soft, "expected MIPS soft-float ABI")
    else:
        raise BuildError(f"unsupported executable format: {kind}")


def build_release(root, targets, output, toolchain=None, native=False):
    require(not output.exists() and not output.is_symlink(), f"output already exists: {output}")
    cargo = ["cargo"] + ([f"+{toolchain}"] if toolchain else [])
    metadata = json.loads(checked_output(cargo + ["metadata", "--locked", "--no-deps", "--format-version", "1"], root))
    versions = {package["version"] for package in metadata["packages"]
                if package["id"] in metadata["workspace_members"]}
    require(len(versions) == 1, "workspace package versions differ")
    version = versions.pop()
    source = source_manifest(root)
    build_id = uuid.uuid4().hex
    env = os.environ.copy()
    env.update(REMOTE_OPS_SOURCE_SHA256=source["sha256"], REMOTE_OPS_RELEASE_BUILD_ID=build_id)
    manifest = {"format_version": 1, "version": version, "protocol_version": 4,
                "build_id": build_id, "created_at": datetime.now(timezone.utc).isoformat(),
                "source": source, "artifacts": [],
                "toolchains": {"rustc": checked_output(["rustc"] + ([f"+{toolchain}"] if toolchain else []) + ["-vV"], root).strip()},
                "targets": [target["target"] for target in targets]}
    if any("spec" in target for target in targets):
        manifest["toolchains"]["nightly"] = checked_output(["rustc", "+nightly", "-vV"], root).strip()
    if not native and any("spec" not in target for target in targets):
        manifest["toolchains"]["zig"] = checked_output(["zig", "version"], root).strip()
        manifest["toolchains"]["cargo_installed"] = checked_output(cargo + ["install", "--list"], root).strip()
    matrix = json.loads((root / "targets/releases.json").read_text(encoding="utf-8"))
    manifest["complete_matrix"] = {t["target"] for t in targets} == {t["target"] for t in matrix}
    output.parent.mkdir(parents=True, exist_ok=True)
    # Keep the directory beside the output for an atomic rename, with a stable
    # ignored name so root-level builds do not contaminate Git dirty metadata.
    staging = Path(tempfile.mkdtemp(prefix=".remote-ops-release-", dir=output.parent))
    try:
        for target in targets:
            artifacts = compile_target(root, target, toolchain, native, env)
            expected = {"version": version, "target": target["target"], "profile": "release",
                        "source_sha256": source["sha256"], "build_id": build_id}
            for name, path in sorted(artifacts.items()):
                require(path.is_absolute() and path.is_file() and not path.is_symlink(),
                        f"Cargo executable is not an absolute regular file: {path}")
                destination = staging / (name + "-" + target["suffix"])
                shutil.copy2(path, destination)
                data = destination.read_bytes()
                check_binary(data, target)
                embedded = read_metadata(data, expected)
                destination.chmod(destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
                manifest["artifacts"].append({"name": destination.name, "target": target["target"],
                                              "size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                                              "metadata": embedded})
            print("Verified " + target["target"], flush=True)
        require(source_manifest(root) == source, "source changed while building; refusing mixed release")
        (staging / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")
        sums = [(item["name"], item["sha256"]) for item in manifest["artifacts"]]
        sums.append(("manifest.json", sha256_file(staging / "manifest.json")))
        (staging / "SHA256SUMS").write_text("".join(f"{digest}  {name}\n" for name, digest in sorted(sums)), encoding="ascii")
        require(not output.exists() and not output.is_symlink(), f"output appeared during build: {output}")
        staging.rename(output)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
    print(f"Release verified: {output} ({len(manifest['artifacts'])} binaries)", flush=True)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", action="append", help="repeat to select a subset of targets")
    parser.add_argument("--output", type=Path, help="new output directory; never overwritten")
    parser.add_argument("--toolchain", help="Rust toolchain for regular targets; MIPS always uses Nightly")
    parser.add_argument("--native", action="store_true", help="use cargo build instead of cargo zigbuild")
    parser.add_argument("--list-targets", action="store_true")
    args = parser.parse_args()
    matrix = json.loads((ROOT / "targets/releases.json").read_text(encoding="utf-8"))
    if args.list_targets:
        for entry in matrix:
            print(entry["target"])
        return 0
    selected = set(args.target or [entry["target"] for entry in matrix])
    require(selected <= {entry["target"] for entry in matrix}, "unknown release target")
    targets = [entry for entry in matrix if entry["target"] in selected]
    # Read version with Cargo rather than maintaining a second release version.
    cargo = ["cargo"] + ([f"+{args.toolchain}"] if args.toolchain else [])
    metadata = json.loads(checked_output(cargo + ["metadata", "--locked", "--no-deps", "--format-version", "1"], ROOT))
    version = next(package["version"] for package in metadata["packages"] if package["name"] == "remote-ops-agent")
    output = (args.output or ROOT / "Releases" / ("v" + version)).absolute()
    build_release(ROOT, targets, output, args.toolchain, args.native)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (BuildError, OSError, ValueError) as error:
        print(f"Release failed: {error}", file=sys.stderr)
        sys.exit(1)
