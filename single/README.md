# Minecraft: Browser single-file launcher

`launch.svg` and `horizon-launch-sw.js` serve the single-file build of the game
from Google Drive through a service worker, adding the COOP/COEP headers the
game's Web Workers need.

Open the launcher **from jsDelivr**:

<https://cdn.jsdelivr.net/gh/retrograde-sdk/horizon-single@298988611/launch.svg>

The game is not in this repository. It is shared from Google Drive as
anyone-with-link, and the API key in `launch.svg` is restricted to the
`cdn.jsdelivr.net` referrer: opened from another host the key is refused, which
is why the jsDelivr URL above is the way in.

- file id: `1Pi-9SLO6z0dl7qwl_N77Q0fc9aVt7KCY`
- build: `ba419177fff3` (first 12 hex digits of the file's SHA-256)
- size: 356,941,194 bytes

This build samples a cell's centre as well as its four corners, so terrain finer
than a coarse LOD cell — a knoll, a gully, a cliff edge — is drawn instead of
merging into a smooth ramp.

The launcher downloads the file once, keeps it in that origin's IndexedDB, and
lets the worker serve it as `__horizon.html` with:

    Cross-Origin-Opener-Policy: same-origin
    Cross-Origin-Embedder-Policy: require-corp

Later visits go straight to LAUNCH; the bytes never leave the browser again.

`launch.svg?src=<url>` points at another copy of the single file for testing;
that copy must send CORS headers and allow range requests. Even then the page
has to be opened from a host the key allows, or from jsDelivr with `?src=` naming
a CORS-enabled copy.

Rebuilding: `python3 web/package.py single` in the game's source tree, upload the
result to Drive as anyone-with-link, then update `DRIVE_ID` in `launch.svg` and
`BUILD` in `horizon-launch-sw.js` (both carry the build id, so a cached worker is
replaced rather than reused), commit, and point the link at the new commit.

Pin the link to a commit, not `@main`: jsDelivr caches a branch's resolution for
hours, while a commit's files are immutable and serve immediately.

## Credits

The bundled **Visual Vibrance** shader pack is by Josh Britain (jbritain),
© 2025, and is redistributed unmodified for personal, non-commercial use under
its own source-available license (not OSI open source). See the `LICENSE` inside
`shaderpacks/Visual-Vibrance-v0.3.5a.zip` and `THIRD_PARTY_NOTICES.md` in the
source repository. <https://jbritain.net>
