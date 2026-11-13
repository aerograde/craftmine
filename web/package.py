#!/usr/bin/env python3
"""Packages the browser build for players.

  web/out/horizon-web/     the web folder: upload it as is (GitHub Pages,
                           Netlify, any server). Its service worker gives the
                           page the headers the game's threads need; where it
                           can't, the page runs the single-threaded build
                           (pkg-st/).
  web/out/horizon.html     one file, every resource inside it: both builds, all
                           languages, the shader packs and the sounds (the music
                           re-encoded much smaller). It is entirely offline; only
                           the game's multiplayer sockets use the network. Opened
                           from the disk it runs the single-threaded build.
  web/out/launch.svg       with launch-sw.js, serves horizon.html from a static
                           host with the isolation headers, so the game can use
                           its workers there.
  web/out/horizon-offline.html
                           the old lazy single-threaded file, which fetches the
                           rest of the resources when it reads them.

Usage:
  web/package.py [folder|offline|single|all] [--no-build] [--sounds none|effects|all] [--languages]
  web/package.py single [--audio off|tiny|low|medium|high] [--builds both|st|threaded]

It builds first (web/build.sh into web/dist, and THREADS=0 into web/dist-st,
whose cargo target is target/web-st); PROFILE=fast and the other build.sh
variables pass through. `single` needs ffmpeg for the music (without it the
original sounds stay). --sounds and --languages pick what `offline` holds:
every sound but the music and music discs by default (the music is 250 MB), and
English only. `single` always holds everything. The web folder always has
everything: its files are fetched when the game first reads them.
"""

import argparse
import base64
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

WEB = Path(__file__).resolve().parent
REPO = WEB.parent
sys.path.insert(0, str(WEB))
import bundle  # noqa: E402

DIST = WEB / "dist"
DIST_ST = WEB / "dist-st"
PAGE_FILES = ("index.html", "main.js", "audio.js", "worker.js", "sw.js")
LAUNCHER_FILES = ("launch.svg", "launch-sw.js")

# The single file's sounds are re-encoded as Vorbis (symphonia reads ogg/vorbis;
# it has no Opus). Music and records get the heavy setting, effects a lighter
# one. -1 is Vorbis's lowest quality, around 45 kbit/s stereo.
AUDIO_QUALITY = {
    "tiny": ("-1", "0"),
    "low": ("0", "1"),
    "medium": ("2", "3"),
    "high": ("4", "5"),
}
AUDIO_CACHE_VERSION = 1


def build(threaded: bool, resources: bool) -> None:
    env = dict(os.environ)
    if not threaded:
        env.update(THREADS="0", DIST=str(DIST_ST), CARGO_TARGET_DIR=str(REPO / "target/web-st"))
    if not resources:
        env["BUNDLE"] = "0"
    subprocess.run([str(WEB / "build.sh")], env=env, check=True)


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()[:16]


def folder(out: Path) -> None:
    site = out / "horizon-web"
    if site.exists():
        shutil.rmtree(site)
    site.mkdir(parents=True)
    for name in PAGE_FILES:
        shutil.copy2(WEB / name, site / name)
    shutil.copytree(DIST / "pkg", site / "pkg")
    shutil.copytree(DIST_ST / "pkg", site / "pkg-st")
    if (DIST / "packs").is_dir():
        shutil.copytree(DIST / "packs", site / "packs")
    fs = site / "fs"
    fs.mkdir()
    for path in (DIST / "fs").iterdir():
        if path.is_file():
            shutil.copy2(path, fs / path.name)
    # The files fetched one by one: a packed folder's come with its pack.
    res, count = DIST / "fs" / "res", 0
    for line in (fs / "manifest.txt").read_text().splitlines():
        rel, *rest = line.split("\t")
        if len(rest) > 1:
            continue
        (fs / "res" / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(res / rel, fs / "res" / rel)
        count += 1
    files = ("pkg/horizon-client_bg.wasm", "pkg-st/horizon-client_bg.wasm", "fs/boot.tar.gz")
    (site / "build.json").write_text("{" + ", ".join(f'"{f}": "{digest(site / f)}"' for f in files) + "}\n")
    size = sum(p.stat().st_size for p in site.rglob("*") if p.is_file())
    print(f"{site}: {size / 1e6:.0f} MB ({count} files fetched when read)")


def held(rel: str, sounds: str, languages: bool) -> bool:
    """Whether the offline file holds a resource the web folder fetches when read."""
    parts = rel.split("/")
    if parts[:3] == ["assets", "minecraft", "sounds"]:
        return sounds == "all" or (sounds == "effects" and parts[3] not in ("music", "records"))
    if parts[:3] == ["assets", "minecraft", "lang"]:
        return languages
    return True


def gz(data: bytes) -> bytes:
    return gzip.compress(data, compresslevel=9, mtime=0)


def tar_bytes(files: list[tuple[str, Path]]) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tar:
        for rel, path in files:
            tar.add(path, arcname=rel, recursive=False)
    return raw.getvalue()


def data_url(js: str) -> str:
    return "data:text/javascript;base64," + base64.b64encode(js.encode()).decode()


def glue_url(pkg: Path) -> str:
    """The game's glue as one data: URL module, its snippets inlined as data: URLs."""
    glue = (pkg / "horizon-client.js").read_text(encoding="utf-8")
    snippets = {}

    def inline(m: re.Match) -> str:
        rel = m.group(2)
        snippets.setdefault(rel, data_url((pkg / rel).read_text(encoding="utf-8")))
        return f"from {m.group(1)}{snippets[rel]}{m.group(1)}"

    # Only the imports: the module's import names keep the paths.
    glue = re.sub(r"""from\s*(['"])\./(snippets/[^'"]+\.js)\1""", inline, glue)
    return data_url(glue)


OFFLINE_LOADER = """
// The offline page's files: base64 in the script elements below the page,
// each read once and dropped from the page.
function held(id) {
    const element = document.getElementById(id);
    const text = element.textContent;
    element.remove();
    if (Uint8Array.fromBase64) return Uint8Array.fromBase64(text);
    const binary = atob(text);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
}

async function gunzip(bytes) {
    return new Uint8Array(await new Response(
        new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer());
}

globalThis.horizonOffline = async () => {
    status.textContent = "Loading the game…";
    const glue = await import(GLUE);
    // Runs the game's main, which only registers the game to start.
    await glue.default({ module_or_path: await gunzip(held("horizon-wasm")) });
    status.textContent = "Unpacking resources…";
    const tar = await gunzip(held("horizon-boot"));
    // The files the game reads later: one block, cut by the manifest's lengths.
    const store = await gunzip(held("horizon-store"));
    const manifest = document.getElementById("horizon-manifest").textContent;
    const index = new Map();
    let at = 0;
    for (const line of manifest.split("\\n")) {
        if (!line) continue;
        const [rel, length] = line.split("\\t");
        index.set(rel, [at, Number(length)]);
        at += Number(length);
    }
    // horizon-os's horizon_fetch_sync asks here before it fetches.
    globalThis.horizonHeldFile = (url) => {
        const file = url.startsWith("held:") && index.get(url.slice(5));
        return file ? store.subarray(file[0], file[0] + file[1]) : undefined;
    };
    return { glue, tar, manifest, base: "held:" };
};
"""


def without_audio_import(main: str) -> str:
    """main.js's audio wrapper becomes a classic script before the module."""
    stripped, count = re.subn(r'import\s+["\']\./audio\.js["\'];?[^\n]*\n', "", main, count=1)
    if not count:
        sys.exit("main.js no longer imports audio.js as package.py expects")
    return stripped


def offline(out: Path, resources: Path, sounds: str, languages: bool) -> None:
    boot, store = [], []
    for dirpath, _, names in os.walk(resources):
        for name in names:
            path = Path(dirpath) / name
            rel = path.relative_to(resources).as_posix()
            if not bundle.lazy(rel) or rel.split("/")[0] in bundle.PACKED_DIRS:
                boot.append((rel, path))
            elif held(rel, sounds, languages):
                store.append((rel, path))
    boot.sort()
    store.sort()
    manifest = "".join(f"{rel}\t{path.stat().st_size}\n" for rel, path in store)

    def block(id: str, data: bytes) -> str:
        return f'<script type="application/octet-stream" id="{id}">{base64.b64encode(data).decode()}</script>\n'

    pkg = st_package(DIST_ST)
    page = (WEB / "index.html").read_text(encoding="utf-8")
    main = without_audio_import((WEB / "main.js").read_text(encoding="utf-8"))
    script = f"<script>\n{(WEB / 'audio.js').read_text(encoding='utf-8')}</script>\n" + \
        f'<script type="module">\nconst GLUE = "{glue_url(pkg)}";\n{OFFLINE_LOADER}\n{main}</script>\n'
    tag = '<script type="module" src="main.js"></script>\n'
    if tag not in page:
        sys.exit("index.html no longer loads main.js as package.py expects")
    head, tail = page.split(tag)
    target = out / "horizon-offline.html"
    with open(target, "w", encoding="utf-8") as f:
        f.write(head)
        f.write(block("horizon-wasm", gz((pkg / "horizon-client_bg.wasm").read_bytes())))
        f.write(block("horizon-boot", gz(tar_bytes(boot))))
        f.write(block("horizon-store", gz(b"".join(path.read_bytes() for _, path in store))))
        f.write(f'<script type="text/plain" id="horizon-manifest">{manifest}</script>\n')
        f.write(script)
        f.write(tail)
    print(f"{target}: {target.stat().st_size / 1e6:.0f} MB ({len(boot)} files, {len(store)} held for later)")


def resource_files(resources: Path) -> list[tuple[str, Path]]:
    """Every prepared file, sorted, symlinks resolved and OS clutter dropped."""
    files = []
    seen_dirs = set()
    for dirpath, dirnames, names in os.walk(resources, followlinks=True):
        real = Path(dirpath).resolve()
        if real in seen_dirs:
            dirnames[:] = []
            continue
        seen_dirs.add(real)
        for name in names:
            if name in (".DS_Store", "Thumbs.db"):
                continue
            path = Path(dirpath) / name
            files.append((path.relative_to(resources).as_posix(), path.resolve() if path.is_symlink() else path))
    files.sort()
    return files


def is_music(rel: str) -> bool:
    return tuple(rel.split("/")[:4]) in (
        ("assets", "minecraft", "sounds", "music"),
        ("assets", "minecraft", "sounds", "records"),
    )


def audio_profile(rel: str, quality: str) -> str | None:
    """The Vorbis quality a sound is re-encoded at, or None to keep its bytes."""
    if quality == "off" or not rel.lower().endswith(".ogg"):
        return None
    music, effects = AUDIO_QUALITY[quality]
    return music if is_music(rel) else effects


_VORBIS_ENCODER: str | None = None

# ffmpeg's native Vorbis encoder has a much narrower quality scale than
# libvorbis (and only writes stereo), so the profiles are remapped for it.
NATIVE_VORBIS_QUALITY = {"-1": "0", "0": "1", "1": "1", "2": "2", "3": "4", "4": "6", "5": "8"}


def vorbis_encoder() -> str:
    """Which Vorbis encoder this machine has: oggenc, ffmpeg's libvorbis, or ffmpeg's own."""
    global _VORBIS_ENCODER
    if _VORBIS_ENCODER is not None:
        return _VORBIS_ENCODER
    if shutil.which("oggenc") and shutil.which("ffmpeg"):
        _VORBIS_ENCODER = "oggenc"
        return _VORBIS_ENCODER
    if shutil.which("ffmpeg"):
        try:
            encoders = subprocess.run(
                ["ffmpeg", "-hide_banner", "-encoders"], capture_output=True, text=True
            ).stdout
        except Exception:
            encoders = ""
        if re.search(r"^\s*\S+\s+libvorbis\b", encoders, re.M):
            _VORBIS_ENCODER = "ffmpeg-libvorbis"
            return _VORBIS_ENCODER
        if re.search(r"^\s*\S+\s+vorbis\b", encoders, re.M):
            _VORBIS_ENCODER = "ffmpeg-vorbis"
            return _VORBIS_ENCODER
    _VORBIS_ENCODER = ""
    return _VORBIS_ENCODER


def transcode_audio(source: Path, profile: str, cache: Path, encoder: str) -> Path | None:
    """A cached Vorbis copy of one sound, or None to keep the original."""
    digest = hashlib.sha256()
    with open(source, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    target = cache / f"{digest.hexdigest()}-q{profile}-{encoder}-v{AUDIO_CACHE_VERSION}.ogg"
    if target.is_file() and target.stat().st_size:
        return target
    with tempfile.NamedTemporaryFile(dir=cache, prefix=".horizon-audio-", suffix=".ogg", delete=False) as tmp:
        tmp_path = Path(tmp.name)
    try:
        if encoder == "oggenc":
            # oggenc encodes PCM, not Vorbis: ffmpeg decodes the sound and the
            # encoder reads the WAV from the pipe.
            tmp_path.unlink(missing_ok=True)
            decode = subprocess.Popen(
                ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
                 "-i", str(source), "-f", "wav", "-"],
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            )
            encode = ["oggenc", "-Q", "-q", profile]
            if profile == "-1":
                encode += ["--downmix", "--resample", "24000"]
            encode += ["-o", str(tmp_path), "-"]
            try:
                encoded = subprocess.run(encode, stdin=decode.stdout, capture_output=True)
            finally:
                decode.stdout.close()
                decode.wait()
            if decode.returncode != 0:
                raise RuntimeError("ffmpeg could not decode the sound")
            if encoded.returncode != 0:
                raise RuntimeError(encoded.stderr.decode(errors="replace").strip() or "oggenc failed")
            result = encoded
        else:
            command = [
                "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error", "-y",
                "-i", str(source), "-vn", "-map_metadata", "-1", "-threads", "1",
            ]
            if encoder == "ffmpeg-libvorbis":
                command += ["-c:a", "libvorbis", "-q:a", profile]
                if profile == "-1":
                    command += ["-ac", "1", "-ar", "24000"]
            else:
                quality = NATIVE_VORBIS_QUALITY.get(profile, profile)
                command += ["-ac", "2", "-strict", "experimental", "-c:a", "vorbis", "-q:a", quality]
                if profile == "-1":
                    command += ["-ar", "24000"]
            command += ["-f", "ogg", str(tmp_path)]
            result = subprocess.run(command, capture_output=True)
        if result.returncode != 0 or not tmp_path.stat().st_size:
            raise RuntimeError(result.stderr.decode(errors="replace").strip() or "empty output")
        if tmp_path.stat().st_size >= source.stat().st_size:
            tmp_path.unlink(missing_ok=True)
            return None
        os.replace(tmp_path, target)
        return target
    except Exception as e:
        tmp_path.unlink(missing_ok=True)
        print(f"warning: {source}: {e}", file=sys.stderr)
        return None


def prepare_resources(resources: Path, quality: str, cache: Path) -> tuple[list[tuple[str, Path]], int, int]:
    """Every resource with its audio re-encoded; returns the files, count and bytes saved."""
    files = resource_files(resources)
    audio = [(rel, path) for rel, path in files if audio_profile(rel, quality)]
    if not audio:
        return files, 0, 0
    encoder = vorbis_encoder()
    if not encoder:
        print("warning: no Vorbis encoder found (oggenc with ffmpeg, or an ffmpeg with libvorbis); "
              "the single file keeps the original sounds", file=sys.stderr)
        return files, 0, 0
    cache.mkdir(parents=True, exist_ok=True)
    print(f"Re-encoding {len(audio)} sounds to Vorbis with {encoder} (quality {quality})…")
    by_rel = dict(files)
    replaced = saved = 0
    with ThreadPoolExecutor(max_workers=min(8, os.cpu_count() or 2)) as pool:
        futures = [pool.submit(transcode_audio, path, audio_profile(rel, quality), cache, encoder)
                   for rel, path in audio]
        for done, (rel, path), future in zip(range(1, len(audio) + 1), audio, futures):
            out = future.result()
            if out is not None:
                by_rel[rel] = out
                replaced += 1
                saved += path.stat().st_size - out.stat().st_size
            if done % 50 == 0 or done == len(audio):
                print(f"\r  {done}/{len(audio)} sounds", end="", flush=True)
    print()
    print(f"  {replaced} re-encoded, {saved / 1e6:.1f} MB smaller")
    return sorted(by_rel.items()), replaced, saved


def add_tar_file(tar: tarfile.TarFile, arcname: str, path: Path) -> None:
    info = tar.gettarinfo(str(path), arcname=arcname)
    info.mtime = 0
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mode = 0o644
    with open(path, "rb") as f:
        tar.addfile(info, f)


def single_archive(out_path: Path, files: list[tuple[str, Path]]) -> None:
    """One deterministic tar.gz of every resource."""
    with open(out_path, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=9, mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
            for rel, path in files:
                add_tar_file(tar, rel, path)


def packs_archive(out_path: Path, packs: list[tuple[str, Path]]) -> None:
    """The shader packs, unpacked into `/shaderpacks` beside the resources.

    The prepared-resource archive is installed under the game's resource root,
    where the game never looks for packs; the settings scan `/shaderpacks`.
    """
    with open(out_path, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=9, mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
            for name, path in packs:
                add_tar_file(tar, f"shaderpacks/{name}", path)


def dist_resources(dist: Path) -> Path:
    """A prepared-resource tree rebuilt from a packaged `web/dist` folder.

    Lets `single` be rebuilt on a machine that no longer has
    `.local/horizon-client/26.3`, as long as `web/dist/fs` holds the archives.
    """
    root = Path(tempfile.mkdtemp(prefix=".horizon-resources-", dir=dist.parent))
    fs = dist / "fs"
    for name in ("boot.tar.gz", "worldgen.tar.gz", "biome-parameters.tar.gz"):
        archive = fs / name
        if archive.is_file():
            with tarfile.open(archive, "r:gz") as tar:
                try:
                    tar.extractall(root, filter="data")
                except TypeError:  # Python before 3.12 has no extraction filter
                    tar.extractall(root)
    lazy = fs / "res"
    if lazy.is_dir():
        shutil.copytree(lazy, root, dirs_exist_ok=True)
    return root


def st_package(threaded_dist: Path, single_dist: Path | None = None) -> Path:
    """The single-threaded build: web/dist-st/pkg, or web/dist/pkg-st."""
    for candidate in (threaded_dist / "pkg", (single_dist or DIST) / "pkg-st"):
        if (candidate / "horizon-client_bg.wasm").is_file():
            return candidate
    return threaded_dist / "pkg"


def gzip_file(source: Path, target: Path) -> None:
    with open(target, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=9, mtime=0) as gz:
        with open(source, "rb") as src:
            shutil.copyfileobj(src, gz, 1 << 20)


def write_base64_block(out, element_id: str, path: Path) -> None:
    """Writes a file as base64 in a script element, a block at a time."""
    out.write(f'<script type="application/octet-stream" id="{element_id}">')
    with open(path, "rb") as source:
        carry = b""
        while True:
            block = source.read(3 << 20)
            if not block:
                break
            data = carry + block
            cut = len(data) - len(data) % 3
            out.write(base64.b64encode(data[:cut]).decode("ascii"))
            carry = data[cut:]
        if carry:
            out.write(base64.b64encode(carry).decode("ascii"))
    out.write("</script>\n")


def pack_files() -> list[tuple[str, Path]]:
    """The shader packs to put in the single file, its own folder first."""
    packs: dict[str, Path] = {}
    for folder in (WEB / "packs", DIST / "packs"):
        if not folder.is_dir():
            continue
        for path in sorted(folder.glob("*.zip")):
            packs.setdefault(path.name, path)
    return sorted(packs.items())


SINGLE_LOADER = r"""
// The single file's own builds and resources. Nothing below leaves this file:
// only the game's multiplayer sockets reach the network.
globalThis.horizonOfflineFilesOnly = true;
const GLUE_ST = __GLUE_ST__;
const GLUE_THREADED = __GLUE_THREADED__;
const WORKER_SOURCE = __WORKER_SOURCE__;

function held(id) {
    const element = document.getElementById(id);
    const text = element.textContent;
    element.remove();
    if (Uint8Array.fromBase64) return Uint8Array.fromBase64(text);
    const binary = atob(text);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
}

async function gunzip(bytes) {
    return new Uint8Array(await new Response(
        new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer());
}

function threadedBuild() {
    return GLUE_THREADED && WORKER_SOURCE && globalThis.crossOriginIsolated
        && typeof SharedArrayBuffer !== "undefined";
}

// The game asks for worker.js beside the page. In one file there is no such
// worker: hand every spawn the same blob, whose module is this file's glue.
function patchWorkers() {
    const NativeWorker = globalThis.Worker;
    const workerUrl = URL.createObjectURL(new Blob([WORKER_SOURCE], { type: "text/javascript" }));
    globalThis.Worker = class extends NativeWorker {
        constructor(target, options) {
            let path = "";
            try { path = new URL(String(target), location.href).pathname; } catch (e) {}
            super(path.endsWith("/worker.js") ? workerUrl : target, options);
        }
    };
}

globalThis.horizonOffline = async () => {
    const threaded = threadedBuild();
    if (!threaded && !GLUE_ST) {
        throw new Error("This file holds only the threaded build. Open launch.svg from a static host " +
            "(or serve this file with Cross-Origin-Opener-Policy: same-origin and " +
            "Cross-Origin-Embedder-Policy: require-corp) so the game can use its workers.");
    }
    if (threaded) patchWorkers();
    // The build that wasn't picked stays as base64 text in the page: drop it.
    const unused = document.getElementById(threaded ? "horizon-wasm" : "horizon-wasm-threaded");
    if (unused) unused.remove();
    status.textContent = threaded ? "Starting Horizon with workers…" : "Starting Horizon…";
    const glue = await import(threaded ? GLUE_THREADED : GLUE_ST);
    await glue.default({
        module_or_path: await gunzip(held(threaded ? "horizon-wasm-threaded" : "horizon-wasm")),
    });
    status.textContent = "Unpacking all resources…";
    const tar = await gunzip(held("horizon-boot"));
    // Shader packs live in `/shaderpacks`, not under the resource root.
    const packs = document.getElementById("horizon-packs")
        ? await gunzip(held("horizon-packs")) : null;
    return { glue, tar, packs, manifest: "", base: "held:" };
};
"""


def single(
    out: Path,
    resources: Path,
    builds: str,
    audio: str,
    packs: list[tuple[str, Path]],
    launcher: bool,
    dist: Path | None = None,
    dist_st: Path | None = None,
) -> None:
    """One HTML file holding every resource and both builds (web/out/horizon.html)."""
    if not (resources / "horizon-resources.json").is_file():
        sys.exit(f"{resources} holds no prepared resources: run horizon-prepare first, or set HORIZON_RESOURCES")
    # Resolved here, not as defaults: tests and callers replace the module's
    # DIST/DIST_ST, and a default evaluated at import ignored that.
    dist = dist or DIST
    dist_st = dist_st or DIST_ST
    want_st = builds in ("both", "st")
    want_threaded = builds in ("both", "threaded")
    st, threaded = st_package(dist_st, dist), dist / "pkg"
    if want_st and not (st / "horizon-client_bg.wasm").is_file():
        sys.exit(f"{st} holds no single-threaded build: run web/build.sh (THREADS=0) first")
    if want_threaded and not (threaded / "horizon-client_bg.wasm").is_file():
        sys.exit(f"{threaded} holds no threaded build: run web/build.sh first")

    files, replaced, saved = prepare_resources(resources, audio, WEB / "out" / ".audio-cache")
    # A file advertised as holding everything must not be silently silent or
    # untranslated: say which classes the prepared tree lacks.
    classes = {
        "sounds": "assets/minecraft/sounds/",
        "languages": "assets/minecraft/lang/",
        "world generation": "worldgen/",
    }
    missing = [
        name
        for name, prefix in classes.items()
        if not any(rel.startswith(prefix) for rel, _ in files)
    ]
    if missing:
        print(f"  warning: this resource tree has no {', '.join(missing)}: "
              "run horizon-prepare sounds/languages, then rebuild")
    out.mkdir(parents=True, exist_ok=True)
    target = out / "horizon.html"
    with tempfile.TemporaryDirectory(dir=out, prefix=".horizon-single-") as temp:
        boot = Path(temp) / "boot.tar.gz"
        single_archive(boot, files)
        packs_tar = None
        if packs:
            packs_tar = Path(temp) / "packs.tar.gz"
            packs_archive(packs_tar, packs)

        glue_st = glue_url(st) if want_st else None
        glue_threaded = glue_url(threaded) if want_threaded else None
        worker_source = None
        if want_threaded:
            worker = (WEB / "worker.js").read_text(encoding="utf-8")
            needle = 'from "./pkg/horizon-client.js"'
            if needle not in worker:
                sys.exit("web/worker.js no longer imports ./pkg/horizon-client.js as package.py expects")
            worker_source = ("globalThis.horizonOfflineFilesOnly = true;\n"
                             + worker.replace(needle, f"from {json.dumps(glue_threaded)}"))

        page = (WEB / "index.html").read_text(encoding="utf-8")
        main = without_audio_import((WEB / "main.js").read_text(encoding="utf-8"))
        tag = '<script type="module" src="main.js"></script>\n'
        if tag not in page:
            sys.exit("index.html no longer loads main.js as package.py expects")
        head, tail = page.split(tag)
        loader = (SINGLE_LOADER
                  .replace("__GLUE_ST__", json.dumps(glue_st))
                  .replace("__GLUE_THREADED__", json.dumps(glue_threaded))
                  .replace("__WORKER_SOURCE__", json.dumps(worker_source)))

        built = Path(temp) / "horizon.html"
        with open(built, "w", encoding="utf-8") as f:
            f.write(head)
            f.write(f"<script>\n{(WEB / 'audio.js').read_text(encoding='utf-8')}</script>\n")
            if want_st:
                wasm_st = Path(temp) / "wasm-st.gz"
                gzip_file(st / "horizon-client_bg.wasm", wasm_st)
                write_base64_block(f, "horizon-wasm", wasm_st)
            if want_threaded:
                wasm_threaded = Path(temp) / "wasm-threaded.gz"
                gzip_file(threaded / "horizon-client_bg.wasm", wasm_threaded)
                write_base64_block(f, "horizon-wasm-threaded", wasm_threaded)
            write_base64_block(f, "horizon-boot", boot)
            if packs_tar:
                write_base64_block(f, "horizon-packs", packs_tar)
            f.write("<script type=\"module\">\n")
            f.write(loader)
            f.write(main)
            f.write("</script>\n")
            f.write(tail)
        os.replace(built, target)

    if launcher:
        for name in LAUNCHER_FILES:
            source = WEB / name
            if source.is_file():
                shutil.copy2(source, out / name)
    size = target.stat().st_size
    which = "both builds" if want_st and want_threaded else ("the threaded build" if want_threaded else "the single-threaded build")
    print(f"{target}: {size / 1e6:.0f} MB ({len(files)} resources, {len(packs)} packs, "
          f"{which}, {replaced} sounds re-encoded, {saved / 1e6:.1f} MB saved)")
    if launcher and (out / "launch.svg").is_file() and want_threaded:
        print(f"{out / 'launch.svg'}: open it on a static host to give this file its workers")
    if want_threaded and not want_st:
        print("This file holds only the threaded build: it needs launch.svg (or COOP/COEP headers).")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("what", nargs="?", choices=("folder", "offline", "single", "all"), default="all")
    parser.add_argument("--no-build", action="store_true", help="package what web/dist and web/dist-st hold")
    parser.add_argument("--sounds", choices=("none", "effects", "all"), default="effects",
                        help="offline: which sounds the lazy file holds")
    parser.add_argument("--languages", action="store_true", help="offline: every language, not only English")
    parser.add_argument("--audio", choices=("off", "tiny", "low", "medium", "high"), default="low",
                        help="single: Vorbis quality the sounds are re-encoded at (default low)")
    parser.add_argument("--builds", choices=("both", "st", "threaded"), default="both",
                        help="single: which WebAssembly builds to embed")
    parser.add_argument("--no-packs", action="store_true", help="single: leave the shader packs out")
    parser.add_argument("--no-launcher", action="store_true", help="single: don't copy launch.svg and launch-sw.js")
    parser.add_argument("--out", type=Path, default=WEB / "out")
    parser.add_argument("--dist", type=Path, default=DIST, help="the threaded build folder")
    parser.add_argument("--dist-st", type=Path, default=DIST_ST, help="the single-threaded build folder")
    parser.add_argument("--from-dist", action="store_true",
                        help="rebuild single's resources from --dist/fs (no .local tree needed)")
    args = parser.parse_args()
    resources = REPO / os.environ.get("HORIZON_RESOURCES", ".local/horizon-client/26.3")
    packaged = None
    if args.from_dist:
        packaged = dist_resources(args.dist)
        resources = packaged
    packs = [] if args.no_packs else pack_files()
    if not args.no_build:
        if args.what in ("folder", "all"):
            build(threaded=True, resources=True)
            build(threaded=False, resources=False)
        if args.what == "offline":
            build(threaded=False, resources=False)
        if args.what == "single":
            if args.builds in ("both", "threaded"):
                build(threaded=True, resources=False)
            if args.builds in ("both", "st"):
                build(threaded=False, resources=False)
    args.out.mkdir(parents=True, exist_ok=True)
    if args.what in ("folder", "all"):
        folder(args.out)
    if args.what == "offline":
        offline(args.out, resources, args.sounds, args.languages)
    try:
        if args.what in ("single", "all"):
            single(args.out, resources, "both" if args.what == "all" else args.builds,
                   args.audio, packs, not args.no_launcher, args.dist, args.dist_st)
    finally:
        if packaged is not None:
            shutil.rmtree(packaged, ignore_errors=True)


if __name__ == "__main__":
    main()
