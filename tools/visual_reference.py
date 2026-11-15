#!/usr/bin/env python3
"""Capture black-box visual reference frames and compare two renderings.

The reference pack is never read, copied or committed by this tool. Point it at
a game already running with the reference pack installed (or with Horizon's
built-in profile), then capture the same poses from both builds:

    python3 tools/visual_reference.py capture --socket /tmp/a.sock --out .local/ref
    python3 tools/visual_reference.py capture --socket /tmp/b.sock --out .local/new
    python3 tools/visual_reference.py compare --reference .local/ref --candidate .local/new

The pose file is JSON: a list of objects with `name`, `pos`, optional `look`,
optional `time`, and optional `wait` game ticks. Captures live under `.local/`
and are not tracked.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_POSES = ROOT / "tools" / "visual-reference-poses.json"


def control(ctl: Path, socket: Path, *args: str) -> None:
    subprocess.run(
        [str(ctl), "--socket", str(socket), *args],
        check=True,
        stdout=subprocess.DEVNULL,
    )


def load_poses(path: Path) -> list[dict]:
    poses = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(poses, list) or not poses:
        raise SystemExit(f"{path}: expected a non-empty JSON list")
    names = set()
    for pose in poses:
        name = pose.get("name")
        if not isinstance(name, str) or not name or name in names:
            raise SystemExit(f"{path}: every pose needs a unique non-empty name")
        names.add(name)
        if not (isinstance(pose.get("pos"), list) and len(pose["pos"]) == 3):
            raise SystemExit(f"{path}: {name}: pos must be [x, y, z]")
        for key in ("look", "time", "wait"):
            if key in pose:
                if key == "look":
                    if not (isinstance(pose[key], list) and len(pose[key]) == 2):
                        raise SystemExit(f"{path}: {name}: look must be [yaw, pitch]")
                elif not isinstance(pose[key], int):
                    raise SystemExit(f"{path}: {name}: {key} must be an integer")
    return poses


def capture(args: argparse.Namespace) -> None:
    poses = load_poses(args.poses)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    for pose in poses:
        x, y, z = pose["pos"]
        control(args.ctl, args.socket, "tp", str(x), str(y), str(z))
        if "look" in pose:
            yaw, pitch = pose["look"]
            control(args.ctl, args.socket, "look", str(yaw), str(pitch))
        if "time" in pose:
            control(args.ctl, args.socket, "time", str(pose["time"]))
        control(args.ctl, args.socket, "wait", str(pose.get("wait", 20)))
        target = out / f"{pose['name']}.png"
        control(args.ctl, args.socket, "screenshot", str(target))
        print(f"captured {target}")


def compare(args: argparse.Namespace) -> None:
    try:
        from PIL import Image, ImageChops, ImageStat
    except ImportError:
        raise SystemExit("compare needs Pillow: python3 -m pip install pillow")
    reference = sorted(args.reference.glob("*.png"))
    candidate = sorted(args.candidate.glob("*.png"))
    if not reference:
        raise SystemExit(f"{args.reference}: no PNG captures")
    if [p.name for p in reference] != [p.name for p in candidate]:
        raise SystemExit("reference and candidate capture names differ")
    worst = 0.0
    failures = []
    for left_path, right_path in zip(reference, candidate):
        left = Image.open(left_path).convert("RGB")
        right = Image.open(right_path).convert("RGB")
        if left.size != right.size:
            failures.append(f"{left_path.name}: size {left.size} != {right.size}")
            continue
        # Mean absolute channel error, normalised to 0..1. This is cheap and
        # stable across GPU vendors; it is a parity smoke test, not a metric of
        # aesthetic quality.
        delta = ImageChops.difference(left, right)
        mae = sum(ImageStat.Stat(delta).mean) / (3.0 * 255.0)
        worst = max(worst, mae)
        print(f"{left_path.name}: mean absolute error {mae:.4f}")
        if mae > args.max_mae:
            failures.append(f"{left_path.name}: {mae:.4f} > {args.max_mae:.4f}")
    print(f"worst mean absolute error: {worst:.4f}")
    if failures:
        raise SystemExit("\n".join(failures))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    cap = sub.add_parser("capture", help="drive a running game and save PNGs")
    cap.add_argument("--ctl", type=Path, default=ROOT / "target" / "fast" / "horizon-ctl")
    cap.add_argument("--socket", type=Path, required=True)
    cap.add_argument("--poses", type=Path, default=DEFAULT_POSES)
    cap.add_argument("--out", type=Path, required=True)
    cap.set_defaults(run=capture)

    cmp = sub.add_parser("compare", help="compare two capture directories")
    cmp.add_argument("--reference", type=Path, required=True)
    cmp.add_argument("--candidate", type=Path, required=True)
    cmp.add_argument("--max-mae", type=float, default=0.03)
    cmp.set_defaults(run=compare)

    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    sys.exit(main())
