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

- file id: `1GTf4LaN75PMx2_-i4UO30JfYOCLbKUf_`
- build: `56b3ee752040` (first 12 hex digits of the file's SHA-256)
- size: 355,503,474 bytes

The title screen names the commit it was built from in its top-left corner
(`Build 24c6431e01f7`), so a screenshot says which build it is. A shader pack's
GLSL translation now runs on a worker instead of the frame's thread: it used to
compile inside the first in-world frame, so a world went live and then froze for
6-10 seconds on a desktop (minutes on a small laptop) with nothing drawing. The
loading overlay stays up while it translates. It also culls occluded sections in
the browser (the `InShader` test was limited to Vulkan and Metal, so every
browser drew every hidden section of terrain), and loads the world tables off
the page's own thread.

This build has sound: `cpal` asks the `AudioContext` to resume as soon as the
game opens its stream, which is before the player has clicked or typed
anything, and the browser refuses that. The refusal used to fail the open and
drop the stream, so every browser build was silent for good. The stream is kept
now, and `web/audio.js` resumes the context on the first gesture.

Play Nearby works too. `horizon-nearby` was not a green thread, so the service
quietly never started on the single-threaded build and no one was ever seen
nearby; it also re-resolves its room once the page's address lookup lands
(previously it stayed in the public room, where nobody from its own network
is), and auto-joins the first open world it finds.

It samples a cell's centre as well as its four corners, so terrain finer than a
coarse LOD cell — a knoll, a gully, a cliff edge — is drawn instead of merging
into a smooth ramp. And it opens single-player worlds in the browser: before
that, a world sat at "Loading terrain..." with the scene frozen behind the
loader (see `web/README.md`).

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
