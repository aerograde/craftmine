"use strict";
// Serves the single-file game that launch.svg stored in this origin's IndexedDB,
// with the two headers that make the page cross-origin isolated. The game's
// threaded build needs them before it may share memory between Web Workers.
// The launcher asks this worker for BUILD, so a stale copy on a CDN is noticed.

const DB_NAME = "horizon-launcher";
const DB_VERSION = 1;
const GAME_NAME = "__horizon.html";
const BUILD = "ba419177fff3";
const GAME_PATH = new URL(GAME_NAME, self.registration.scope).pathname;

const POLICY = {
    "Content-Type": "text/html; charset=utf-8",
    "Cross-Origin-Opener-Policy": "same-origin",
    "Cross-Origin-Embedder-Policy": "require-corp",
    "Cross-Origin-Resource-Policy": "same-origin",
    "Cache-Control": "no-store",
    "X-Content-Type-Options": "nosniff",
    "Accept-Ranges": "bytes",
};

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("message", (event) => {
    const port = event.ports && event.ports[0];
    if (port && event.data && event.data.q === "build") port.postMessage(BUILD);
});

function openDb() {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(DB_NAME, DB_VERSION);
        request.onupgradeneeded = () => {
            const db = request.result;
            if (!db.objectStoreNames.contains("meta")) db.createObjectStore("meta");
            if (!db.objectStoreNames.contains("chunks")) db.createObjectStore("chunks");
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
    });
}

function readOnly(db, store, key) {
    return new Promise((resolve, reject) => {
        const request = db.transaction(store, "readonly").objectStore(store).get(key);
        request.onsuccess = () => resolve(request.result || null);
        request.onerror = () => reject(request.error);
    });
}

// null = the whole body, [start, end) = a 206, "invalid" = 416. Like a plain
// server, a multi-range or malformed header gets the whole body.
function parseRange(header, size) {
    if (!header) return null;
    const match = /^bytes=(\d*)-(\d*)$/.exec(String(header).trim());
    if (!match) return null;
    if (match[1] === "" && match[2] === "") return null;
    let start;
    let end;
    if (match[1] === "") {
        const fromEnd = parseInt(match[2], 10);
        if (!fromEnd) return "invalid";
        start = Math.max(0, size - fromEnd);
        end = size;
    } else {
        start = parseInt(match[1], 10);
        end = match[2] === "" ? size : Math.min(size, parseInt(match[2], 10) + 1);
    }
    if (start >= size || start >= end) return "invalid";
    return [start, end];
}

// The chunks are Blobs, and the whole game is a Blob that names them. The
// browser keeps Blob payloads out of the JavaScript heap (on disk in Chrome),
// which is what lets a 100–200 MB file be served without loading it twice.
let BLOB_CACHE = null;

function blobFor(db, meta) {
    const stamp = meta.storedAt || meta.size;
    if (BLOB_CACHE && BLOB_CACHE.stamp === stamp) return Promise.resolve(BLOB_CACHE.blob);
    const count = meta.chunks || 0;
    return new Promise((resolve, reject) => {
        if (!count) {
            const empty = new Blob([]);
            BLOB_CACHE = { stamp, blob: empty };
            resolve(empty);
            return;
        }
        const parts = new Array(count);
        let left = count;
        const store = db.transaction("chunks", "readonly").objectStore("chunks");
        for (let i = 0; i < count; i++) {
            const request = store.get(i);
            request.onsuccess = () => {
                const value = request.result;
                if (value == null) {
                    reject(new Error(`the stored game is missing part ${i}; install it again`));
                    return;
                }
                parts[i] = value instanceof Blob ? value : new Blob([value]);
                left -= 1;
                if (left) return;
                const blob = new Blob(parts);
                BLOB_CACHE = { stamp, blob };
                resolve(blob);
            };
            request.onerror = () => reject(request.error);
        }
    });
}

const NOT_INSTALLED = "Nothing is installed yet. Open launch.svg and press Download.";

function serve(request) {
    return openDb()
        .then((db) => readOnly(db, "meta", "game").then((meta) => {
            if (!meta || !meta.size) {
                return new Response(NOT_INSTALLED, {
                    status: 404,
                    headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" },
                });
            }
            const size = meta.size;
            const range = parseRange(request.headers.get("range"), size);
            if (range === "invalid") {
                return new Response("Range not satisfiable", {
                    status: 416,
                    headers: { ...POLICY, "Content-Range": `bytes */${size}` },
                });
            }
            if (!range) {
                const headers = { ...POLICY, "Content-Length": String(size) };
                if (request.method === "HEAD") return new Response(null, { status: 200, headers });
                return blobFor(db, meta).then((blob) => new Response(blob, { status: 200, headers }));
            }
            const [start, end] = range;
            const headers = {
                ...POLICY,
                "Content-Length": String(end - start),
                "Content-Range": `bytes ${start}-${end - 1}/${size}`,
            };
            if (request.method === "HEAD") return new Response(null, { status: 206, headers });
            return blobFor(db, meta).then((blob) => new Response(blob.slice(start, end), { status: 206, headers }));
        }))
        .catch((error) => new Response(`Horizon launcher: ${error && error.message ? error.message : error}`, {
            status: 500,
            headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" },
        }));
}

self.addEventListener("fetch", (event) => {
    if (event.request.method !== "GET" && event.request.method !== "HEAD") return;
    const url = new URL(event.request.url);
    if (url.origin !== self.location.origin) return;
    if (url.pathname !== GAME_PATH) return;
    event.respondWith(serve(event.request));
});
