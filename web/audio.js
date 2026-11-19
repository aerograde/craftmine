// Browsers keep sound off until the player clicks or presses a key: the
// game's audio contexts (cpal's) start suspended and are resumed then. Loaded
// before the game's glue, which keeps the AudioContext class it finds.
const contexts = [];
if (globalThis.AudioContext) {
    // Not "once": the first gesture is not always honoured (a click that lands
    // on the canvas before the user agent counts it, another tab's focus), so
    // every pointer and key event is a chance until one works.
    const resume = () => {
        for (const ctx of contexts) {
            if (ctx.state !== "suspended") continue;
            ctx.resume().catch(() => {});
        }
    };
    const BrowserAudioContext = globalThis.AudioContext;
    globalThis.AudioContext = class extends BrowserAudioContext {
        constructor(...args) {
            super(...args);
            contexts.push(this);
            // cpal asks to resume as soon as the game opens its stream, which
            // happens before any click: the browser rejects that, and the game
            // then never retries. The policy allows a resume inside a gesture,
            // so take the first one the player makes.
            resume();
        }
    };
    for (const kind of ["pointerdown", "pointerup", "keydown", "keyup", "touchstart", "mousedown"]) {
        addEventListener(kind, resume, true);
    }
    // And a slow poll, for the case where the context is created by a gesture
    // that the events above missed (a synthetic one, say).
    setInterval(resume, 500);
}
