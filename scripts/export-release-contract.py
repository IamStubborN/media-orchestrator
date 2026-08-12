#!/usr/bin/env python3
"""Export the private media release contract as a validated directory."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import uuid


SHA256 = re.compile(r"^[0-9a-f]{64}$")
IMMUTABLE_IMAGE = re.compile(r"^\S+@sha256:[0-9a-f]{64}$")
MIGRATION = re.compile(r"^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$")


class ContractError(RuntimeError):
    pass


def command(*args: str, root: pathlib.Path, environment: dict[str, str] | None = None) -> str:
    try:
        result = subprocess.run(
            args,
            cwd=root,
            env=environment,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = error.stderr.strip() if isinstance(error, subprocess.CalledProcessError) else str(error)
        raise ContractError(f"command failed: {' '.join(args)}: {detail}") from error
    return result.stdout.strip()


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: pathlib.Path, value: object) -> None:
    path.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )


def fsync_file(path: pathlib.Path) -> None:
    with path.open("rb") as source:
        os.fsync(source.fileno())


def fsync_directory(path: pathlib.Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def load_json(path: pathlib.Path, label: str) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ContractError(f"{label} is missing or invalid: {path}") from error


def validate_tool_names(schema_tools: object, capabilities: object) -> None:
    if not isinstance(schema_tools, list) or not all(isinstance(tool, dict) for tool in schema_tools):
        raise ContractError("generated MCP schema must be an array of tool objects")
    schema_names = [tool.get("name") for tool in schema_tools]
    if not all(isinstance(name, str) and name for name in schema_names):
        raise ContractError("generated MCP schema contains an invalid tool name")
    if not isinstance(capabilities, dict) or capabilities.get("schema_version") != 1:
        raise ContractError("media capability manifest must use schema version 1")
    capability_names = capabilities.get("tools")
    if not isinstance(capability_names, list) or not all(
        isinstance(name, str) and name for name in capability_names
    ):
        raise ContractError("media capability manifest contains invalid tool names")
    if len(schema_names) != len(set(schema_names)) or len(capability_names) != len(set(capability_names)):
        raise ContractError("MCP schema and capability tool names must be unique")
    if schema_names != capability_names:
        raise ContractError("MCP schema and capability tool names differ")


def publish(generation: pathlib.Path, destination: pathlib.Path, replace: bool) -> None:
    if destination.exists() and not replace:
        raise ContractError(f"destination already exists; pass --replace to replace it: {destination}")
    if destination.exists() and not destination.is_dir():
        raise ContractError(f"destination is not a directory: {destination}")

    backup: pathlib.Path | None = None
    try:
        if destination.exists():
            backup = destination.parent / f".{destination.name}.previous.{uuid.uuid4().hex}"
            os.replace(destination, backup)
        os.replace(generation, destination)
        fsync_directory(destination.parent)
    except BaseException:
        if backup is not None and backup.exists() and not destination.exists():
            os.replace(backup, destination)
            fsync_directory(destination.parent)
        raise
    if backup is not None:
        shutil.rmtree(backup)


def export(args: argparse.Namespace) -> None:
    root = pathlib.Path(__file__).resolve().parents[1]
    destination = pathlib.Path(args.output).resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)

    if not IMMUTABLE_IMAGE.fullmatch(args.service_image):
        raise ContractError("service image must be an immutable name@sha256 reference")
    if not IMMUTABLE_IMAGE.fullmatch(args.runner_image):
        raise ContractError("runner image must be an immutable name@sha256 reference")
    if not MIGRATION.fullmatch(args.migration_version):
        raise ContractError("migration version is missing or invalid")
    if command("git", "status", "--porcelain", "--untracked-files=all", root=root):
        raise ContractError("Git worktree is dirty")

    source_revision = command("git", "rev-parse", "HEAD", root=root)
    if not re.fullmatch(r"[0-9a-f]{40}", source_revision):
        raise ContractError("source revision is invalid")
    docker_build = str(root / "scripts" / "docker-build.sh")
    source_digest = command(docker_build, "--print-source-tree-digest", root=root)
    application_version = command(docker_build, "--print-source-version", root=root)
    runner_digest = command(docker_build, "--print-runner-build-digest", root=root)
    if not SHA256.fullmatch(source_digest) or not SHA256.fullmatch(runner_digest):
        raise ContractError("build scripts returned an invalid SHA-256 digest")
    if not application_version or application_version.endswith("-dirty"):
        raise ContractError("source version is missing or dirty")

    cli = pathlib.Path(args.cli).resolve()
    checksum_file = pathlib.Path(args.cli_checksum).resolve()
    if not cli.is_file() or not checksum_file.is_file():
        raise ContractError("CLI artifact and checksum must both be regular files")
    checksum_parts = checksum_file.read_text(encoding="ascii").strip().split()
    if not checksum_parts or not SHA256.fullmatch(checksum_parts[0]):
        raise ContractError("CLI checksum file is invalid")
    if len(checksum_parts) > 1 and pathlib.Path(checksum_parts[-1].lstrip("*")).name != cli.name:
        raise ContractError("CLI checksum names a different artifact")
    cli_digest = sha256(cli)
    if checksum_parts[0] != cli_digest:
        raise ContractError("CLI checksum does not match the artifact")

    capabilities = load_json(root / "config" / "media-capabilities.json", "media capability manifest")
    generation = pathlib.Path(
        tempfile.mkdtemp(prefix=f".{destination.name}.generation.", dir=destination.parent)
    )
    try:
        raw_schema = generation / ".MCP_SCHEMA.raw.json"
        environment = os.environ.copy()
        environment["MCP_SCHEMA_SNAPSHOT"] = str(raw_schema)
        command(
            "cargo",
            "test",
            "--locked",
            "-p",
            "media-api",
            "--test",
            "mcp",
            "stateless_mcp_2026_lists_tools_without_initialize_or_session",
            "--",
            "--exact",
            root=root,
            environment=environment,
        )
        schema_tools = load_json(raw_schema, "generated MCP schema")
        validate_tool_names(schema_tools, capabilities)
        raw_schema.unlink()

        schema_path = generation / "MCP_SCHEMA.json"
        capabilities_path = generation / "media-capabilities.json"
        checksum_path = generation / "media-linux-amd64.sha256"
        write_json(
            schema_path,
            {"schema_version": 1, "source_digest": source_digest, "tools": schema_tools},
        )
        write_json(capabilities_path, capabilities)
        checksum_path.write_text(f"{cli_digest}  media-linux-amd64\n", encoding="ascii")

        files = {}
        for path in (schema_path, capabilities_path, checksum_path):
            fsync_file(path)
            files[path.name] = {"sha256": sha256(path)}
        release_path = generation / "release.json"
        write_json(
            release_path,
            {
                "application_version": application_version,
                "files": files,
                "migration_version": args.migration_version,
                "runner_build_digest": runner_digest,
                "runner_image": args.runner_image,
                "schema_version": 1,
                "service_image": args.service_image,
                "source_revision": source_revision,
                "source_tree_digest": source_digest,
            },
        )
        fsync_file(release_path)
        for name, metadata in files.items():
            if sha256(generation / name) != metadata["sha256"]:
                raise ContractError(f"bundle hash validation failed: {name}")
        fsync_directory(generation)
        publish(generation, destination, args.replace)
    finally:
        if generation.exists():
            shutil.rmtree(generation)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-image", required=True)
    parser.add_argument("--runner-image", required=True)
    parser.add_argument("--migration-version", required=True)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--cli-checksum", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--replace", action="store_true")
    return parser.parse_args()


def main() -> int:
    try:
        export(parse_args())
    except (ContractError, OSError, UnicodeError) as error:
        print(f"release contract export failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
