"""web/package.py single: every resource and build inside one HTML file (no linker)."""

import base64
import gzip
import importlib.util
import io
import re
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

WEB = Path(__file__).resolve().parent.parent / "web"
SPEC = importlib.util.spec_from_file_location("web_package", WEB / "package.py")
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


def block(html: str, element_id: str) -> bytes:
    start = html.index(f'id="{element_id}">') + len(f'id="{element_id}">')
    end = html.index("</script>", start)
    return gzip.decompress(base64.b64decode(html[start:end]))


class SingleFileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.resources = self.root / "resources"
        (self.resources / "assets" / "minecraft" / "lang").mkdir(parents=True)
        (self.resources / "assets" / "minecraft" / "sounds" / "music").mkdir(parents=True)
        (self.resources / "horizon-resources.json").write_text("{}")
        (self.resources / "assets" / "minecraft" / "lang" / "en_us.json").write_text('{"key":"value"}')
        (self.resources / "assets" / "minecraft" / "sounds" / "music" / "calm.ogg").write_bytes(b"OGG")
        self.pack = self.root / "pack.zip"
        self.pack.write_bytes(b"PACK")
        self.old = (package.WEB, package.DIST, package.DIST_ST)
        package.WEB = WEB
        package.DIST = self.root / "dist"
        package.DIST_ST = self.root / "dist-st"
        for dist, wasm in ((package.DIST, b"THREADED"), (package.DIST_ST, b"SINGLE")):
            pkg = dist / "pkg"
            (pkg / "snippets" / "snip").mkdir(parents=True)
            (pkg / "horizon-client.js").write_text(
                "import { x } from './snippets/snip/inline.js';\nexport default async function init() {}\n")
            (pkg / "snippets" / "snip" / "inline.js").write_text("export function x() {}\n")
            (pkg / "horizon-client_bg.wasm").write_bytes(wasm)

    def tearDown(self):
        package.WEB, package.DIST, package.DIST_ST = self.old
        self.temp.cleanup()

    def build(self, builds="both"):
        out = self.root / "out"
        package.single(out, self.resources, builds, "off", [("test.zip", self.pack)], True)
        return out, (out / "horizon.html").read_text()

    def cli(self, *argv):
        calls = []
        saved = (package.build, package.folder, package.offline, package.single)
        old_argv = sys.argv
        package.build = lambda threaded, resources: calls.append(("build", threaded, resources))
        package.folder = lambda out: calls.append(("folder",))
        package.offline = lambda out, resources, sounds, languages: calls.append(("offline",))
        package.single = lambda out, resources, builds, audio, packs, launcher, dist, dist_st: \
            calls.append(("single", builds))
        sys.argv = ["package.py", *argv, "--out", str(self.root / "cli-out")]
        try:
            package.main()
        finally:
            package.build, package.folder, package.offline, package.single = saved
            sys.argv = old_argv
        return calls

    def test_cli_builds_exactly_what_each_mode_needs(self):
        self.assertEqual(self.cli("folder"), [
            ("build", True, True), ("build", False, False), ("folder",)])
        self.assertEqual(self.cli("offline"), [
            ("build", False, False), ("offline",)])
        self.assertEqual(self.cli("single"), [
            ("build", True, False), ("build", False, False), ("single", "both")])
        self.assertEqual(self.cli("single", "--builds", "st"), [
            ("build", False, False), ("single", "st")])
        self.assertEqual(self.cli("single", "--builds", "threaded"), [
            ("build", True, False), ("single", "threaded")])
        self.assertEqual(self.cli("all"), [
            ("build", True, True), ("build", False, False), ("folder",), ("single", "both")])

    def test_every_resource_and_both_builds_are_inside(self):
        out, html = self.build()
        self.assertNotIn('src="main.js"', html)
        self.assertNotIn('src="audio.js"', html)
        self.assertIn("Browsers keep sound off", html)
        self.assertLess(html.index("<script>\n"), html.index('<script type="module">'))
        self.assertIn("globalThis.horizonOffline", html)
        self.assertIn("globalThis.horizonOfflineFilesOnly = true;", html)
        self.assertIn("const GLUE_ST =", html)
        self.assertIn("const GLUE_THREADED =", html)
        self.assertIn("WORKER_SOURCE", html)
        self.assertNotIn("./snippets/", html)
        self.assertTrue((out / "launch.svg").is_file())
        self.assertTrue((out / "launch-sw.js").is_file())
        self.assertEqual(block(html, "horizon-wasm"), b"SINGLE")
        self.assertEqual(block(html, "horizon-wasm-threaded"), b"THREADED")
        with tarfile.open(fileobj=io.BytesIO(block(html, "horizon-boot")), mode="r:") as tar:
            names = tar.getnames()
            self.assertIn("assets/minecraft/lang/en_us.json", names)
            self.assertIn("horizon-resources.json", names)
            # Sounds are lazy: installed from the held store the first time the
            # game reads them, not unpacked into the VFS at boot.
            self.assertNotIn("assets/minecraft/sounds/music/calm.ogg", names)
            self.assertNotIn("shaderpacks/test.zip", names)
        manifest = html.split('id="horizon-manifest">', 1)[1].split("</script>", 1)[0]
        self.assertIn("assets/minecraft/sounds/music/calm.ogg\t3\n", manifest)
        self.assertEqual(block(html, "horizon-store"), b"OGG")
        # Packs install at `/shaderpacks`, where the settings scan them; inside
        # the resource archive they landed under the resource root and never
        # appeared in the list.
        self.assertIn('horizon_install_tar("/", packs)', html)
        with tarfile.open(fileobj=io.BytesIO(block(html, "horizon-packs")), mode="r:") as tar:
            self.assertEqual(tar.getnames(), ["shaderpacks/test.zip"])
            self.assertEqual(tar.extractfile("shaderpacks/test.zip").read(), b"PACK")
        self.assertEqual(
            block(html, "horizon-boot"),
            block(self.build()[1], "horizon-boot"),
            "the archive must be deterministic",
        )

    def test_st_package_prefers_the_just_built_single_threaded_module(self):
        """A stale `dist-st` must not shadow a fresh `dist/pkg-st`.

        Both layouts exist (a plain THREADS=0 build writes `dist/pkg-st`,
        `DIST=web/dist-st` writes `dist-st/pkg`). The threaded `dist/pkg` used to
        be tried first, and only a file existence was checked — so packaging
        silently shipped whatever was in `dist-st`, once an old module that
        still drew the edition banner.
        """
        def wasm(path):
            (path / "pkg").mkdir(parents=True, exist_ok=True)
            (path / "pkg" / "horizon-client_bg.wasm").write_bytes(b"WASM")
            return path

        threaded = wasm(self.root / "dist")
        stale = wasm(self.root / "dist-st")
        self.assertEqual(package.st_package(stale, threaded), stale / "pkg")
        self.assertEqual(package.st_package(stale, None), stale / "pkg")
        # A `pkg-st` this build wrote outranks the copy in `dist-st`.
        pkg_st = threaded / "pkg-st"
        pkg_st.mkdir(parents=True)
        (pkg_st / "horizon-client_bg.wasm").write_bytes(b"SINGLE")
        self.assertEqual(package.st_package(stale, threaded), pkg_st)
        # Nothing single-threaded anywhere: the threaded module is all there is,
        # and `single()` then refuses to package it as the offline module.
        (pkg_st / "horizon-client_bg.wasm").unlink()
        (stale / "pkg" / "horizon-client_bg.wasm").unlink()
        self.assertEqual(package.st_package(stale, threaded), threaded / "pkg")

    def test_single_threaded_only_leaves_the_threaded_glue_null(self):
        _, html = self.build("st")
        self.assertNotIn('id="horizon-wasm-threaded"', html)
        self.assertIn("const GLUE_ST =", html)
        self.assertIn("const GLUE_THREADED = null;", html)
        self.assertIn("const WORKER_SOURCE = null;", html)

    def test_threaded_only_leaves_the_single_threaded_glue_null(self):
        _, html = self.build("threaded")
        self.assertNotIn('id="horizon-wasm"', html)
        self.assertIn("const GLUE_ST = null;", html)
        self.assertIn("const GLUE_THREADED =", html)
        self.assertIn("WORKER_SOURCE", html)

    def test_launcher_is_well_formed_and_matches_its_worker(self):
        import xml.etree.ElementTree as ET
        ET.parse(WEB / "launch.svg")
        svg = (WEB / "launch.svg").read_text()
        worker = (WEB / "launch-sw.js").read_text()
        self.assertIn('var SW_BUILD = "horizon-single-1"', svg)
        self.assertIn('const BUILD = "horizon-single-1"', worker)
        self.assertIn('"Cross-Origin-Opener-Policy": "same-origin"', worker)
        self.assertIn('"Cross-Origin-Embedder-Policy": "require-corp"', worker)
        # The two must agree: the launcher asks the worker for its BUILD and
        # warns when they differ, so a half-updated pair is a broken deploy.
        build = lambda text, pat: __import__("re").search(pat, text).group(1)
        self.assertEqual(
            build(svg, r'var SW_BUILD = "([^"]+)"'),
            build(worker, r'const BUILD = "([^"]+)"'),
            "launch.svg and horizon-launch-sw.js must name the same build",
        )

    def test_the_launcher_names_the_build_that_is_shipped(self):
        """`single/` must describe the HTML in `web/out`, not an older one.

        The build id is the first 12 hex digits of the file's SHA-256, so this
        can be checked without the 355 MB file: the README states both, and the
        launcher and worker carry the id. A stale id is invisible at runtime
        until the launcher warns, and it once shipped an old module.
        """
        import hashlib
        import re
        html = WEB / "out" / "horizon.html"
        if not html.is_file():
            self.skipTest("no packaged HTML: run web/package.py single")
        digest = hashlib.sha256(html.read_bytes()).hexdigest()[:12]
        repo = WEB.parent
        readme = (repo / "single" / "README.md").read_text()
        self.assertIn(
            f"`{digest}`", readme, "single/README.md names a build that is not the shipped one"
        )
        svg = (repo / "single" / "launch.svg").read_text()
        self.assertEqual(re.search(r'var SW_BUILD = "([^"]+)"', svg).group(1), digest)
        worker = (repo / "single" / "horizon-launch-sw.js").read_text()
        self.assertEqual(re.search(r'const BUILD = "([^"]+)"', worker).group(1), digest)
        # And the size the README claims is the file's.
        size = f"{html.stat().st_size:,}"
        self.assertIn(size, readme, "single/README.md states the wrong size")

    def test_the_embedded_blobs_are_streamed_not_read_whole(self):
        """A gunzip that reads the whole result keeps two copies in the heap.

        The resource store is 220 MB plain and the page holds its base64 too,
        so an `arrayBuffer()` on the decompressed stream doubles the largest
        allocation on the boot path. The loader must read it in parts and drop
        the compressed side as it goes.
        """
        # The source, not a built page: an old `web/out` on disk would let a
        # whole-read pass here while the shipped file still had it.
        self.build()
        for handler in re.findall(r"async function gunzip\(bytes\) \{.*?\n\}", package.OFFLINE_LOADER, re.S):
            self.assertNotIn("arrayBuffer()", handler, "reads the whole result")
            self.assertIn("getReader()", handler, "reads it in parts")
            self.assertIn("bytes = null", handler, "drops the compressed copy")
        self.assertGreaterEqual(package.OFFLINE_LOADER.count("async function gunzip"), 1)

    def test_audio_profiles(self):
        self.assertIsNone(package.audio_profile("assets/minecraft/sounds/music/a.ogg", "off"))
        self.assertIsNone(package.audio_profile("assets/minecraft/textures/a.png", "low"))
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/music/a.ogg", "low"), "0")
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/random/a.ogg", "low"), "1")
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/records/a.ogg", "tiny"), "-1")


if __name__ == "__main__":
    unittest.main()
