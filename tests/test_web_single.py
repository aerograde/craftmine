"""web/package.py single: every resource and build inside one HTML file (no linker)."""

import base64
import gzip
import importlib.util
import io
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

    def test_audio_profiles(self):
        self.assertIsNone(package.audio_profile("assets/minecraft/sounds/music/a.ogg", "off"))
        self.assertIsNone(package.audio_profile("assets/minecraft/textures/a.png", "low"))
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/music/a.ogg", "low"), "0")
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/random/a.ogg", "low"), "1")
        self.assertEqual(package.audio_profile("assets/minecraft/sounds/records/a.ogg", "tiny"), "-1")


if __name__ == "__main__":
    unittest.main()
