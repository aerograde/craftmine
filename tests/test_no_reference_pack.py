"""The reference shader pack must never be tracked, packaged or selected."""

import hashlib
import os
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACK_NAMES = {"visual-vibrance-v0.3.5a.zip"}
PACK_SHA256 = "0bf94c357b7d3a7312aa828edf892354c25273931543d0087b5577aad4fb76f0"
SKIP = {".git", ".local", "target", "web/out", "web/dist", "web/dist-st", "node_modules"}
# These two files contain the denylist itself, not pack-specific code.
ALLOW = {"tests/test_no_reference_pack.py", "tools/check_public_tree.py"}


def source_files():
    for directory, names, files in os.walk(ROOT):
        rel_dir = Path(directory).relative_to(ROOT).as_posix()
        names[:] = [
            name
            for name in names
            if not any(
                (rel_dir == "." and name == skip)
                or rel_dir == skip
                or rel_dir.startswith(f"{skip}/")
                or (rel_dir == "." and name in {s.split("/")[0] for s in SKIP})
                for skip in SKIP
            )
        ]
        for name in files:
            path = Path(directory) / name
            rel = path.relative_to(ROOT).as_posix()
            if any(rel == skip or rel.startswith(f"{skip}/") for skip in SKIP):
                continue
            yield path, rel


class NoReferencePackTests(unittest.TestCase):
    def test_no_reference_pack_is_present(self):
        found = [rel for path, rel in source_files() if path.name.lower() in PACK_NAMES]
        self.assertEqual(found, [], "the reference pack must stay outside the repository")

    def test_no_reference_pack_is_embedded(self):
        for path, rel in source_files():
            if path.suffix == ".zip":
                self.assertNotEqual(
                    hashlib.sha256(path.read_bytes()).hexdigest(),
                    PACK_SHA256,
                    f"the reference pack is embedded at {rel}",
                )

    def test_no_pack_specific_code_or_packaging(self):
        needles = ("jbritain", "Visual-Vibrance-v", "shaderpacks/Visual-Vibrance")
        offenders = []
        for path, rel in source_files():
            if rel in ALLOW or path.suffix not in {".rs", ".js", ".py", ".toml", ".md", ".json"}:
                continue
            text = path.read_text(encoding="utf-8", errors="ignore")
            for needle in needles:
                if needle in text:
                    offenders.append(f"{rel}: {needle}")
        self.assertEqual(offenders, [], "pack-specific references remain")


if __name__ == "__main__":
    unittest.main()
