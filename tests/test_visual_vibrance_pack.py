"""The original Visual Vibrance pack must ship byte-identical, with its license."""

import base64
import gzip
import hashlib
import io
import tarfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACK = ROOT / "web" / "dist" / "packs" / "Visual-Vibrance-v0.3.5a.zip"
SINGLE = ROOT / "web" / "out" / "horizon.html"
SHA256 = "0bf94c357b7d3a7312aa828edf892354c25273931543d0087b5577aad4fb76f0"


def embedded_pack(html: str) -> bytes:
    start = html.index('id="horizon-packs">') + len('id="horizon-packs">')
    end = html.index("</script>", start)
    tar = gzip.decompress(base64.b64decode(html[start:end]))
    with tarfile.open(fileobj=io.BytesIO(tar), mode="r:") as archive:
        name = "shaderpacks/Visual-Vibrance-v0.3.5a.zip"
        return archive.extractfile(name).read()


@unittest.skipUnless(PACK.is_file(), "the source pack is not in this checkout")
class VisualVibrancePackTests(unittest.TestCase):
    def test_the_source_pack_is_the_unmodified_original(self):
        data = PACK.read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), SHA256)
        with zipfile.ZipFile(io.BytesIO(data)) as pack:
            self.assertIn("LICENSE", pack.namelist())
            self.assertIn("shaders/program/final.glsl", pack.namelist())

    @unittest.skipUnless(SINGLE.is_file(), "the single file has not been built")
    def test_the_embedded_copy_is_byte_identical(self):
        data = embedded_pack(SINGLE.read_text(encoding="utf-8"))
        self.assertEqual(hashlib.sha256(data).hexdigest(), SHA256)
        self.assertEqual(data, PACK.read_bytes())


if __name__ == "__main__":
    unittest.main()
