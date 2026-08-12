#!/usr/bin/env python3
"""Export the private media release contract as a validated directory."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import ipaddress
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile


SHA256 = re.compile(r"^[0-9a-f]{64}$")
MIGRATION = re.compile(r"^m[0-9]{8}_[0-9]{6}_[a-z0-9_]+$")
PATH_COMPONENT = re.compile(r"^[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*$")
DOMAIN_COMPONENT = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$")


class ContractError(RuntimeError):
    pass


class PriorBundlePreservedError(ContractError):
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


def valid_registry(registry: str) -> bool:
    if registry.startswith("["):
        match = re.fullmatch(r"\[([0-9A-Fa-f:.]+)\](?::([0-9]+))?", registry)
        if match is None:
            return False
        try:
            ipaddress.IPv6Address(match.group(1))
        except ipaddress.AddressValueError:
            return False
        port = match.group(2)
        return port is None or 1 <= int(port) <= 65535

    host, separator, port = registry.rpartition(":")
    if separator:
        if not port.isdigit() or not 1 <= int(port) <= 65535:
            return False
    else:
        host = registry
    return all(DOMAIN_COMPONENT.fullmatch(component) for component in host.split("."))


def valid_immutable_image(image: str) -> bool:
    if image.count("@sha256:") != 1:
        return False
    repository, digest = image.split("@sha256:")
    if len(repository) > 255 or not SHA256.fullmatch(digest):
        return False
    components = repository.split("/")
    if not components or any(not component for component in components):
        return False
    first = components[0]
    has_registry = len(components) > 1 and (
        first == "localhost" or "." in first or ":" in first or first.startswith("[")
    )
    path = components[1:] if has_registry else components
    if has_registry and not valid_registry(first):
        return False
    return bool(path) and all(PATH_COMPONENT.fullmatch(component) for component in path)


def atomic_exchange(source: pathlib.Path, destination: pathlib.Path) -> None:
    if source.parent != destination.parent:
        raise ContractError("atomic directory exchange requires sibling paths")
    libc = ctypes.CDLL(None, use_errno=True)
    source_bytes = os.fsencode(source)
    destination_bytes = os.fsencode(destination)
    if sys.platform == "darwin" and hasattr(libc, "renamex_np"):
        rename = libc.renamex_np
        rename.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint)
        rename.restype = ctypes.c_int
        result = rename(source_bytes, destination_bytes, 0x00000002)
    elif sys.platform.startswith("linux") and hasattr(libc, "renameat2"):
        rename = libc.renameat2
        rename.argtypes = (
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        )
        rename.restype = ctypes.c_int
        result = rename(-100, source_bytes, -100, destination_bytes, 0x00000002)
    else:
        raise ContractError("--replace requires platform support for atomic directory exchange")
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(source), None, str(destination))


def remove_tree(path: pathlib.Path) -> None:
    shutil.rmtree(path)


def cleanup_tree(path: pathlib.Path) -> None:
    try:
        remove_tree(path)
    except OSError as error:
        print(f"warning: could not remove retired release directory {path}: {error}", file=sys.stderr)


def publish(generation: pathlib.Path, destination: pathlib.Path, replace: bool) -> None:
    if destination.exists() and not replace:
        raise ContractError(f"destination already exists; pass --replace to replace it: {destination}")
    if destination.exists() and not destination.is_dir():
        raise ContractError(f"destination is not a directory: {destination}")

    if destination.exists():
        atomic_exchange(generation, destination)
        try:
            fsync_directory(destination.parent)
        except BaseException as publication_error:
            try:
                atomic_exchange(generation, destination)
            except BaseException as rollback_error:
                raise PriorBundlePreservedError(
                    f"release was exchanged but durability failed; prior bundle is preserved at {generation}: "
                    f"rollback failed: {rollback_error}"
                ) from publication_error
            try:
                fsync_directory(destination.parent)
            except OSError:
                pass
            raise
        cleanup_tree(generation)
        return

    os.replace(generation, destination)
    try:
        fsync_directory(destination.parent)
    except BaseException:
        os.replace(destination, generation)
        try:
            fsync_directory(destination.parent)
        except OSError:
            pass
        raise


def export(args: argparse.Namespace) -> None:
    root = pathlib.Path(__file__).resolve().parents[1]
    destination = pathlib.Path(args.output).resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)

    if not valid_immutable_image(args.service_image):
        raise ContractError("service image must be an immutable name@sha256 reference")
    if not valid_immutable_image(args.runner_image):
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
    preserve_generation = False
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
        try:
            publish(generation, destination, args.replace)
        except PriorBundlePreservedError:
            preserve_generation = True
            raise
    finally:
        if generation.exists() and not preserve_generation:
            cleanup_tree(generation)


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
