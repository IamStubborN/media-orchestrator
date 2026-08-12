#!/usr/bin/env python3
"""Behavior tests for the private release-contract exporter."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
EXPORTER = ROOT / "scripts" / "export-release-contract.py"
HEX_A = "a" * 64
HEX_B = "b" * 64
SERVICE_IMAGE = f"registry.example/media-service@sha256:{'1' * 64}"
RUNNER_IMAGE = f"registry.example/media-runner@sha256:{'2' * 64}"
MIGRATION = "m20260810_000040_tracking_claims"


def load_exporter():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("release_contract_exporter", EXPORTER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


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
        runner_image: str = RUNNER_IMAGE,
        replace: bool = False,
        schema_tools: list[dict[str, object]] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        command = [
            "python3",
            str(self.repo / "scripts" / EXPORTER.name),
            "--service-image",
            service_image,
            "--runner-image",
            runner_image,
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

    def test_rejects_invalid_repository_syntax_for_both_images(self) -> None:
        invalid = [
            "",
            f"@sha256:{'3' * 64}",
            f"/media@sha256:{'3' * 64}",
            f"registry.example/@sha256:{'3' * 64}",
            f"https://registry.example/media@sha256:{'3' * 64}",
            f"registry.example/Media@sha256:{'3' * 64}",
            f"registry.example/media!prod@sha256:{'3' * 64}",
            f"registry.example/media:tag@sha256:{'3' * 64}",
            f"registry.example/media;touch@sha256:{'3' * 64}",
            f"registry.example/media value@sha256:{'3' * 64}",
        ]
        for index, value in enumerate(invalid):
            with self.subTest(field="service", value=value):
                result = self._run(
                    pathlib.Path(self.temporary.name) / f"service-{index}", service_image=value
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("service image", result.stderr)
            with self.subTest(field="runner", value=value):
                result = self._run(
                    pathlib.Path(self.temporary.name) / f"runner-{index}", runner_image=value
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("runner image", result.stderr)

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

    def test_ignored_dist_cli_is_compatible_with_clean_worktree_gate(self) -> None:
        (self.repo / ".gitignore").write_text("/dist/\n", encoding="utf-8")
        self._git("add", ".gitignore")
        self._git("commit", "-qm", "ignore release artifacts")
        distribution = self.repo / "dist"
        distribution.mkdir()
        self.cli = distribution / "media-linux-amd64"
        self.cli.write_bytes(b"ignored release cli\n")
        self.checksum = distribution / "media-linux-amd64.sha256"
        self._write_checksum()

        result = self._run(pathlib.Path(self.temporary.name) / "bundle")

        self.assertEqual(result.returncode, 0, result.stderr)


class AtomicPublishTest(unittest.TestCase):
    def setUp(self) -> None:
        self.exporter = load_exporter()
        self.temporary = tempfile.TemporaryDirectory()
        self.parent = pathlib.Path(self.temporary.name)
        self.destination = self.parent / "bundle"
        self.generation = self.parent / ".bundle.generation"
        self.destination.mkdir()
        self.generation.mkdir()
        (self.destination / "old.txt").write_text("old\n", encoding="utf-8")
        (self.generation / "new.txt").write_text("new\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_exchange_failure_leaves_prior_destination_untouched(self) -> None:
        with mock.patch.object(self.exporter, "atomic_exchange", side_effect=OSError("rename fault")):
            with self.assertRaises(OSError):
                self.exporter.publish(self.generation, self.destination, True)
        self.assertTrue((self.destination / "old.txt").is_file())
        self.assertTrue((self.generation / "new.txt").is_file())

    def test_first_publish_rename_failure_leaves_destination_absent(self) -> None:
        shutil.rmtree(self.destination)
        with mock.patch.object(self.exporter.os, "replace", side_effect=OSError("rename fault")):
            with self.assertRaises(OSError):
                self.exporter.publish(self.generation, self.destination, False)
        self.assertFalse(self.destination.exists())
        self.assertTrue((self.generation / "new.txt").is_file())

    def test_parent_fsync_failure_rolls_back_with_atomic_exchange(self) -> None:
        real_exchange = self.exporter.atomic_exchange
        with (
            mock.patch.object(self.exporter, "atomic_exchange", side_effect=real_exchange),
            mock.patch.object(self.exporter, "fsync_directory", side_effect=[OSError("fsync fault"), None]),
        ):
            with self.assertRaises(OSError):
                self.exporter.publish(self.generation, self.destination, True)
        self.assertTrue((self.destination / "old.txt").is_file())
        self.assertTrue((self.generation / "new.txt").is_file())

    def test_failed_rollback_preserves_prior_bundle_at_generation_path(self) -> None:
        real_exchange = self.exporter.atomic_exchange
        exchange_count = 0

        def fail_rollback(source, destination):
            nonlocal exchange_count
            exchange_count += 1
            if exchange_count == 1:
                return real_exchange(source, destination)
            raise OSError("rollback rename fault")

        with (
            mock.patch.object(self.exporter, "atomic_exchange", side_effect=fail_rollback),
            mock.patch.object(self.exporter, "fsync_directory", side_effect=OSError("fsync fault")),
        ):
            with self.assertRaises(self.exporter.PriorBundlePreservedError):
                self.exporter.publish(self.generation, self.destination, True)
        self.assertTrue((self.destination / "new.txt").is_file())
        self.assertTrue((self.generation / "old.txt").is_file())

    def test_first_publish_fsync_failure_restores_absent_destination(self) -> None:
        shutil.rmtree(self.destination)
        with mock.patch.object(self.exporter, "fsync_directory", side_effect=[OSError("fault"), None]):
            with self.assertRaises(OSError):
                self.exporter.publish(self.generation, self.destination, False)
        self.assertFalse(self.destination.exists())
        self.assertTrue((self.generation / "new.txt").is_file())

    def test_cleanup_failure_does_not_turn_committed_exchange_into_failure(self) -> None:
        stderr = io.StringIO()
        with (
            mock.patch.object(self.exporter, "remove_tree", side_effect=OSError("cleanup fault")),
            redirect_stderr(stderr),
        ):
            self.exporter.publish(self.generation, self.destination, True)
        self.assertTrue((self.destination / "new.txt").is_file())
        self.assertTrue((self.generation / "old.txt").is_file())
        self.assertIn("warning: could not remove retired release directory", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
