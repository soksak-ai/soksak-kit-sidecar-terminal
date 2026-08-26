#!/usr/bin/env python3
import argparse
import hashlib
import io
import json
import re
import stat
import tarfile
import urllib.request
from pathlib import Path, PurePosixPath

SIDECAR_ID = "soksak-sidecar-pty"
VERSION = "0.0.6"
REPOSITORY = "https://github.com/soksak-ai/soksak-sidecar-pty"
COMMIT = "0ddb87e1aadc94aa83ca751a80f55c84c5835843"
RELEASE_ROOT = f"{REPOSITORY}/releases/download/v{VERSION}"
RELEASE_DOCUMENT = f"{RELEASE_ROOT}/release.json"
# Bare filename inside the release directory. Same pattern as RELEASE_FILE_RE in
# @soksak-ai/plugin-spec release-primitives.ts.
RELEASE_FILE_RE = re.compile(r"^(?!\.\.?$)[A-Za-z0-9._-]+$")


def download(url: str, limit: int) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:
        if response.status != 200:
            raise ValueError(f"GET {url}: HTTP {response.status}")
        body = response.read(limit + 1)
    if len(body) > limit:
        raise ValueError(f"download exceeds {limit} bytes: {url}")
    return body


def refuse_url(value: object, path: str = "release") -> None:
    # A release document names files, never locations. Locations are derived from
    # (kind, id, version) by the consumer; a url key anywhere is a foreign document.
    if isinstance(value, dict):
        if "url" in value:
            raise ValueError(f"PTY release names a url at {path}")
        for key, item in value.items():
            refuse_url(item, f"{path}.{key}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            refuse_url(item, f"{path}[{index}]")


def artifact_url(artifact: dict) -> str:
    return f"{RELEASE_ROOT}/{artifact['file']}"


def select_artifact(document: dict, target: str) -> dict:
    refuse_url(document)
    if document.get("kind") != "sidecar" or document.get("id") != SIDECAR_ID or document.get("version") != VERSION:
        raise ValueError("PTY release identity is invalid")
    if document.get("source") != {"repository": REPOSITORY, "commit": COMMIT}:
        raise ValueError("PTY release source is invalid")
    if not document.get("evidence"):
        raise ValueError("PTY release has no conformance evidence")
    matches = [artifact for artifact in document.get("artifacts", []) if artifact.get("target") == target]
    if len(matches) != 1:
        raise ValueError(f"PTY release has {len(matches)} artifacts for {target}")
    artifact = matches[0]
    if artifact.get("format") != "tar.gz" or artifact.get("manifest") != "sidecar.json":
        raise ValueError("PTY artifact format is invalid")
    if not isinstance(artifact.get("file"), str) or not RELEASE_FILE_RE.match(artifact["file"]):
        raise ValueError("PTY artifact file is not a bare filename")
    if not isinstance(artifact.get("size"), int) or artifact["size"] <= 0:
        raise ValueError("PTY artifact size is invalid")
    digest = artifact.get("sha256", "")
    if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
        raise ValueError("PTY artifact digest is invalid")
    return artifact


def extract_verified_archive(body: bytes, destination: Path, target: str) -> Path:
    destination.mkdir(parents=True, exist_ok=True)
    files = set()
    with tarfile.open(fileobj=io.BytesIO(body), mode="r:gz") as archive:
        for member in archive.getmembers():
            path = PurePosixPath(member.name.removeprefix("./"))
            if path.is_absolute() or ".." in path.parts or member.issym() or member.islnk():
                raise ValueError(f"unsafe PTY archive entry: {member.name}")
            if member.isdir():
                continue
            if not member.isfile() or not path.parts:
                raise ValueError(f"unsupported PTY archive entry: {member.name}")
            output = destination.joinpath(*path.parts)
            output.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                raise ValueError(f"PTY archive entry has no bytes: {member.name}")
            output.write_bytes(source.read())
            output.chmod(0o700 if member.mode & 0o111 else 0o600)
            files.add(path.as_posix())
    manifest_path = destination / "sidecar.json"
    manifest = json.loads(manifest_path.read_text())
    process = manifest.get("process")
    expected = f"dist/{SIDECAR_ID}{'.exe' if 'windows' in target else ''}"
    if manifest.get("id") != SIDECAR_ID or manifest.get("version") != VERSION or process != expected or process not in files:
        raise ValueError("PTY archive manifest does not match its executable")
    executable = destination.joinpath(*PurePosixPath(process).parts)
    executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
    return executable


def install(target: str, destination: Path) -> Path:
    document = json.loads(download(RELEASE_DOCUMENT, 4 << 20))
    artifact = select_artifact(document, target)
    body = download(artifact_url(artifact), artifact["size"])
    if len(body) != artifact["size"] or hashlib.sha256(body).hexdigest() != artifact["sha256"]:
        raise ValueError("PTY artifact bytes do not match release.json")
    return extract_verified_archive(body, destination, target)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--out", required=True, type=Path)
    arguments = parser.parse_args()
    if not arguments.out.is_absolute():
        raise SystemExit("--out must be absolute")
    print(install(arguments.target, arguments.out))


if __name__ == "__main__":
    main()
