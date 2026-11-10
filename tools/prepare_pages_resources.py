#!/usr/bin/env python3
"""Download the pinned Minecraft files for a CI web build.

This is a GitHub Actions helper, not a game tool. It fetches the official
version JSON, client/server jars, libraries and asset objects for the version
in research-lock.json into a scratch directory. Horizon's preparer then turns
those files into .local/horizon-client/<version>; nothing here is committed.
"""

from __future__ import annotations

import concurrent.futures
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"


def get(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def save(url: str, path: Path, sha1: str | None = None) -> None:
    if path.is_file() and (sha1 is None or hashlib.sha1(path.read_bytes()).hexdigest() == sha1):
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    data = get(url)
    if sha1 is not None and hashlib.sha1(data).hexdigest() != sha1:
        raise SystemExit(f"sha1 mismatch: {url}")
    path.write_bytes(data)


def main() -> int:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ".local/minecraft/26.3")
    lock = json.loads((ROOT / "research-lock.json").read_text())
    version_id = lock["minecraft"]["version"]

    manifest = json.loads(get(MANIFEST))
    entry = next((v for v in manifest["versions"] if v["id"] == version_id), None)
    if entry is None:
        raise SystemExit(f"{version_id} is not in Mojang's version manifest")
    version = json.loads(get(entry["url"]))
    out.mkdir(parents=True, exist_ok=True)
    (out / "version.json").write_bytes(get(entry["url"]))

    downloads = version["downloads"]
    save(downloads["client"]["url"], out / "client.jar", downloads["client"]["sha1"])
    save(downloads["server"]["url"], out / "server.jar", downloads["server"]["sha1"])

    libraries = out / "libraries"
    jobs: list[tuple[str, Path, str | None]] = []
    for library in version.get("libraries", []):
        artifact = library.get("downloads", {}).get("artifact")
        if not artifact:
            continue
        group, name, ver = library["name"].split(":")
        relative = Path(*group.split(".")) / name / ver / Path(artifact["path"]).name
        jobs.append((artifact["url"], libraries / relative, artifact.get("sha1")))

    asset_index = version["assetIndex"]
    assets = out / "assets"
    index_path = assets / "indexes" / f"{asset_index['id']}.json"
    save(asset_index["url"], index_path, asset_index.get("sha1"))
    index = json.loads(index_path.read_text())
    for object_data in index["objects"].values():
        digest = object_data["hash"]
        jobs.append((
            f"https://resources.download.minecraft.net/{digest[:2]}/{digest}",
            assets / "objects" / digest[:2] / digest,
            digest,
        ))

    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
        list(pool.map(lambda job: save(*job), jobs))
    print(f"prepared {version_id}: {len(jobs)} files in {out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
