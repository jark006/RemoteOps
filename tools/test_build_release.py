"""Release safety regressions; run: python -m unittest discover -s tools -p 'test_*.py'."""

import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

import build_release as release


TARGET = {"target": "x86_64-apple-darwin", "suffix": "x64-mac", "format": "macho",
          "machine": 16777223, "bins": ["remote-ops-agent"]}


def macho():
    return struct.pack("<IIII", 0xfeedfacf, 16777223, 3, 2)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "targets").mkdir()
        (self.root / "targets/releases.json").write_text(json.dumps([TARGET]), encoding="utf-8")
        (self.root / "Cargo.toml").write_text("source\n", encoding="utf-8")
        self.output = self.root / "out"
        self.cargo_metadata = json.dumps({"packages": [{"id": "agent", "version": "2.0.0"}],
                                          "workspace_members": ["agent"]})

    def checked_output(self, command, root):
        return self.cargo_metadata if "metadata" in command else "test toolchain"

    def artifact(self, root, target, toolchain, native, env):
        expected = {"version": "2.0.0", "target": target["target"], "profile": "release",
                    "source_sha256": env["REMOTE_OPS_SOURCE_SHA256"],
                    "build_id": env["REMOTE_OPS_RELEASE_BUILD_ID"]}
        path = self.root / "artifact"
        path.write_bytes(macho() + release.MARKER + json.dumps(expected).encode() + release.MARKER_END)
        return {"remote-ops-agent": path}

    def build(self, compiler=None):
        with patch.object(release, "checked_output", side_effect=self.checked_output), \
                patch.object(release, "compile_target", side_effect=compiler or self.artifact):
            return release.build_release(self.root, [TARGET], self.output, native=True)

    def assert_no_release(self):
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.root.glob(".out-*")), [])

    def test_success_hashes_binary_and_manifest(self):
        result = self.build()
        self.assertTrue(result["complete_matrix"])
        for line in (self.output / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ")
            self.assertEqual(digest, release.sha256_file(self.output / name))
        self.assertEqual(result, json.loads((self.output / "manifest.json").read_text()))

    def test_existing_output_is_preserved(self):
        self.output.mkdir()
        sentinel = self.output / "keep"
        sentinel.write_text("untouched")
        with self.assertRaisesRegex(release.BuildError, "already exists"):
            self.build()
        self.assertEqual(sentinel.read_text(), "untouched")

    def test_failed_build_leaves_no_completed_directory(self):
        def fail(*args):
            raise release.BuildError("compiler failed")
        with self.assertRaisesRegex(release.BuildError, "compiler failed"):
            self.build(fail)
        self.assert_no_release()

    def test_source_change_rejects_release(self):
        def changed(*args):
            artifacts = self.artifact(*args)
            (self.root / "Cargo.toml").write_text("changed")
            return artifacts
        with self.assertRaisesRegex(release.BuildError, "source changed"):
            self.build(changed)
        self.assert_no_release()

    def test_old_artifact_is_rejected(self):
        def stale(*args):
            artifacts = self.artifact(*args)
            path = artifacts["remote-ops-agent"]
            path.write_bytes(path.read_bytes().replace(b'"build_id": "', b'"build_id": "stale-'))
            return artifacts
        with self.assertRaisesRegex(release.BuildError, "provenance"):
            self.build(stale)
        self.assert_no_release()

    def test_missing_metadata_is_rejected(self):
        with self.assertRaisesRegex(release.BuildError, "missing embedded"):
            release.read_metadata(macho(), {})

    def test_wrong_architecture_is_rejected(self):
        with self.assertRaisesRegex(release.BuildError, "wrong Mach-O CPU"):
            release.check_binary(macho(), dict(TARGET, machine=16777228))

    def test_dynamic_elf_is_rejected(self):
        data = bytearray(120)
        data[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<HH", data, 16, 2, 62)
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<HH", data, 54, 56, 1)
        struct.pack_into("<I", data, 64, 3)
        with self.assertRaisesRegex(release.BuildError, "PT_INTERP"):
            release.check_binary(data, {"format": "elf", "bits": 64, "machine": 62})

    def test_cargo_failure_even_after_reporting_an_artifact(self):
        process = type("Process", (), {"stdout": io.StringIO(json.dumps({
            "reason": "compiler-artifact", "target": {"name": "remote-ops-agent", "kind": ["bin"]},
            "executable": str(self.root / "stale")}) + "\n"), "wait": lambda self: 1})()
        with patch.object(release.subprocess, "Popen", return_value=process):
            with self.assertRaisesRegex(release.BuildError, "exit 1"):
                release.compile_target(self.root, TARGET, None, True, {})

    def test_cargo_success_without_reported_executable_is_rejected(self):
        # A stale executable exists at the usual path; it must never be collected.
        stale = self.root / "target/x86_64-apple-darwin/release/remote-ops-agent"
        stale.parent.mkdir(parents=True)
        stale.write_bytes(macho())
        process = type("Process", (), {"stdout": io.StringIO(""), "wait": lambda self: 0})()
        with patch.object(release.subprocess, "Popen", return_value=process):
            with self.assertRaisesRegex(release.BuildError, "did not report"):
                release.compile_target(self.root, TARGET, None, True, {})

    def test_mips_never_uses_zigbuild_or_regular_toolchain(self):
        command = release.cargo_command({"spec": "targets/mipsel-unknown-linux-musl.json"}, "1.95", False)
        self.assertEqual(command[:3], ["cargo", "+nightly", "build"])
        self.assertIn("build-std=std,panic_abort", command)


if __name__ == "__main__":
    unittest.main()
