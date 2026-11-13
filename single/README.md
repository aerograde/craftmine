# Horizon single-file launcher

`launch.svg` and `horizon-launch-sw.js` serve the single-file build from Google
Drive through a service worker, adding the COOP/COEP headers the game's Web
Workers need.

The 231,555,026-byte `horizon.html` is not in this repository. It is shared from
Google Drive as anyone-with-link:

- file id: `1o_7XgQ39hX1HHayNF9pLmzn0bbPqRWov`
- build: `ada127908db7` (first 12 hex digits of the file's SHA-256)

Open the launcher from jsDelivr:

<https://cdn.jsdelivr.net/gh/retrograde-sdk/horizon-single@main/launch.svg>

The Google API key in `launch.svg` is restricted to the jsDelivr referrer, so
the launcher must be opened from `cdn.jsdelivr.net`, not copied to another
host. The worker reads the downloaded bytes from IndexedDB and serves
`__horizon.html` from the same origin with `Cross-Origin-Opener-Policy:
same-origin` and `Cross-Origin-Embedder-Policy: require-corp`.

`launch.svg?src=<url>` can point at another copy of the HTML for testing; that
copy must send CORS headers and allow range requests.
