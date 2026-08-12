#!/usr/bin/env python3
"""Behavior tests for the private release-contract exporter."""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
EXPORTER = ROOT / "scripts" / "export-release-contract.py"
HEX_A = "a" * 64
HEX_B = "b" * 64
SERVICE_IMAGE = f"registry.example/media-service@sha256:{'1' * 64}"
RUNNER_IMAGE = f"registry.example/media-runner@sha256:{'2' * 64}"
MIGRATION = "m20260810_000040_tracking_claims"


class ReleaseContractTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.repo = pathlib.Path(self.temporary.name) / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "config").mkdir()
        (self.repo / "bin").mkdir()
        shutil.copy2(EXPORTER, self.repo / "scripts" / EXPORTER.name)
        self._write_executable(
            self.repo / "scripts" / "docker-build.sh",
            f"""#!/bin/sh
case "$1" in
  --print-source-tree-digest) echo {HEX_A} ;;
  --print-source-version) echo v1.2.3 ;;
  --print-runner-build-digest) echo {HEX_B} ;;
  *) exit 2 ;;
esac
""",
        )
        self.schema_tools = [
            {"name": "media_jobs_list", "inputSchema": {"type": "object"}},
            {"name": "media_queue_status", "inputSchema": {"type": "object"}},
        ]
        self._write_json(
            self.repo / "config" / "media-capabilities.json",
            {
                "schema_version": 1,
                "mcp_server": "media_admin",
                "description": "fixture",
                "tools": ["media_jobs_list", "media_queue_status"],
            },
        )
        self._write_executable(
            self.repo / "bin" / "cargo",
            """#!/usr/bin/env python3
import json, os, pathlib
pathlib.Path(os.environ["MCP_SCHEMA_SNAPSHOT"]).write_text(
    os.environ["FAKE_MCP_TOOLS"], encoding="utf-8"
)
""",
        )
        self.cli = self.repo / "media-linux-amd64"
        self.cli.write_bytes(b"fixture cli\n")
        self.checksum = self.repo / "media-linux-amd64.sha256"
        self._write_checksum()
        self._git("init", "-q")
        self._git("config", "user.email", "test@example.invalid")
        self._git("config", "user.name", "Release Test")
        self._git("add", ".")
        self._git("commit", "-qm", "fixture")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_executable(self, path: pathlib.Path, content: str) -> None:
        path.write_text(content, encoding="utf-8")
        path.chmod(0o755)

    def _write_json(self, path: pathlib.Path, value: object) -> None:
        path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")

    def _write_checksum(self, digest: str | None = None) -> None:
        digest = digest or hashlib.sha256(self.cli.read_bytes()).hexdigest()
        self.checksum.write_text(f"{digest}  {self.cli.name}\n", encoding="ascii")

    def _git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=self.repo, check=True)

    def _run(
        self,
        output: pathlib.Path,
        *,
        service_image: str = SERVICE_IMAGE,
        replace: bool = False,
        schema_tools: list[dict[str, object]] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        command = [
            "python3",
            str(self.repo / "scripts" / EXPORTER.name),
            "--service-image",
            service_image,
            "--runner-image",
            RUNNER_IMAGE,
            "--migration-version",
            MIGRATION,
            "--cli",
            str(self.cli),
            "--cli-checksum",
            str(self.checksum),
            "--output",
            str(output),
        ]
        if replace:
            command.append("--replace")
        environment = os.environ.copy()
        environment["PATH"] = f"{self.repo / 'bin'}{os.pathsep}{environment['PATH']}"
        environment["FAKE_MCP_TOOLS"] = json.dumps(
            self.schema_tools if schema_tools is None else schema_tools,
            separators=(",", ":"),
        )
        return subprocess.run(
            command,
            cwd=self.repo,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_output_is_deterministic_and_self_hashed(self) -> None:
        first = pathlib.Path(self.temporary.name) / "first"
        second = pathlib.Path(self.temporary.name) / "second"

        first_result = self._run(first)
        second_result = self._run(second)

        self.assertEqual(first_result.returncode, 0, first_result.stderr)
        self.assertEqual(second_result.returncode, 0, second_result.stderr)
        expected_files = {
            "MCP_SCHEMA.json",
            "media-capabilities.json",
            "media-linux-amd64.sha256",
            "release.json",
        }
        self.assertEqual({path.name for path in first.iterdir()}, expected_files)
        for name in expected_files:
            self.assertEqual((first / name).read_bytes(), (second / name).read_bytes())

        release = json.loads((first / "release.json").read_text(encoding="utf-8"))
        self.assertEqual(release["schema_version"], 1)
        self.assertEqual(release["application_version"], "v1.2.3")
        self.assertEqual(release["source_tree_digest"], HEX_A)
        self.assertEqual(release["runner_build_digest"], HEX_B)
        self.assertEqual(release["migration_version"], MIGRATION)
        self.assertEqual(release["service_image"], SERVICE_IMAGE)
        self.assertEqual(release["runner_image"], RUNNER_IMAGE)
        for name in expected_files - {"release.json"}:
            actual = hashlib.sha256((first / name).read_bytes()).hexdigest()
            self.assertEqual(release["files"][name]["sha256"], actual)
        schema = json.loads((first / "MCP_SCHEMA.json").read_text(encoding="utf-8"))
        self.assertEqual(schema, {"schema_version": 1, "source_digest": HEX_A, "tools": self.schema_tools})

    def test_rejects_mutable_service_image(self) -> None:
        result = self._run(pathlib.Path(self.temporary.name) / "bundle", service_image="media:latest")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("immutable", result.stderr)

    def test_rejects_dirty_worktree(self) -> None:
        (self.repo / "dirty.txt").write_text("dirty\n", encoding="utf-8")
        result = self._run(pathlib.Path(self.temporary.name) / "bundle")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("worktree is dirty", result.stderr)

    def test_rejects_cli_checksum_mismatch(self) -> None:
        self._write_checksum("0" * 64)
        self._git("add", str(self.checksum))
        self._git("commit", "-qm", "wrong checksum fixture")
        result = self._run(pathlib.Path(self.temporary.name) / "bundle")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CLI checksum", result.stderr)

    def test_rejects_schema_capability_drift(self) -> None:
        result = self._run(
            pathlib.Path(self.temporary.name) / "bundle",
            schema_tools=[{"name": "media_jobs_list"}, {"name": "unexpected_tool"}],
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tool names differ", result.stderr)

    def test_rejects_schema_capability_order_drift(self) -> None:
        result = self._run(
            pathlib.Path(self.temporary.name) / "bundle",
            schema_tools=list(reversed(self.schema_tools)),
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tool names differ", result.stderr)

    def test_failure_preserves_existing_destination(self) -> None:
        destination = pathlib.Path(self.temporary.name) / "bundle"
        destination.mkdir()
        marker = destination / "keep.txt"
        marker.write_text("original\n", encoding="utf-8")
        self._write_checksum("0" * 64)
        self._git("add", str(self.checksum))
        self._git("commit", "-qm", "wrong checksum fixture")

        result = self._run(destination, replace=True)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(marker.read_text(encoding="utf-8"), "original\n")
        self.assertEqual({path.name for path in destination.iterdir()}, {"keep.txt"})

    def test_existing_destination_requires_replace(self) -> None:
        destination = pathlib.Path(self.temporary.name) / "bundle"
        destination.mkdir()
        result = self._run(destination)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--replace", result.stderr)


if __name__ == "__main__":
    unittest.main()
