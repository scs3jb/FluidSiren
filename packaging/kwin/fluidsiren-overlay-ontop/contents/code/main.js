// FluidSiren overlay keep-on-top.
//
// The recording overlay is a wlr-layer-shell surface on the `overlay` layer —
// already the highest layer the protocol offers. But other overlay-layer windows
// (notably always-on-top IDEs like cmux) share that layer, and KWin restacks
// within a layer whenever a window is raised: click into such an IDE and it jumps
// above the pill. Nothing the client does over the protocol can prevent that.
//
// So we re-raise the overlay from the compositor side. A short poller runs only
// while the overlay exists and nudges it back to the top of its layer whenever
// something else has been raised over it. Reacting to windowActivated alone is
// not enough — KWin raises the activated window *after* emitting the signal, so a
// poll is what reliably wins.

var OVERLAY_CLASS = "fluidsiren-overlay";
var POLL_MS = 200;

var timer = new QTimer();
timer.interval = POLL_MS;
timer.timeout.connect(raiseIfNeeded);
var polling = false; // KWin's ScriptTimer has no isActive(); track it ourselves.

function windows() {
    return workspace.windowList ? workspace.windowList() : workspace.clientList();
}

// The overlay window, or null. Substring match: KWin reports the layer-shell app
// id via resourceClass.
function overlayWindow() {
    var ws = windows();
    for (var i = 0; i < ws.length; i++) {
        var rc = ws[i].resourceClass || "";
        if (rc.indexOf(OVERLAY_CLASS) >= 0) return ws[i];
    }
    return null;
}

function startPolling() { if (!polling) { timer.start(); polling = true; } }
function stopPolling() { if (polling) { timer.stop(); polling = false; } }

// Raise the overlay only when it isn't already topmost, to avoid pointless
// restacking churn every tick. Stops the poller once the overlay is gone.
function raiseIfNeeded() {
    var ov = overlayWindow();
    if (!ov) { stopPolling(); return; }
    var so = workspace.stackingOrder; // bottom -> top
    if (so.length && so[so.length - 1] === ov) return; // already on top
    workspace.raiseWindow(ov);
}

function refresh() {
    if (overlayWindow()) {
        raiseIfNeeded();
        startPolling();
    } else {
        stopPolling();
    }
}

function onAdded(w) {
    if (w && (w.resourceClass || "").indexOf(OVERLAY_CLASS) >= 0) refresh();
}

// KWin 6 uses windowAdded/windowRemoved; older builds use clientAdded/clientRemoved.
(workspace.windowAdded || workspace.clientAdded).connect(onAdded);
(workspace.windowRemoved || workspace.clientRemoved).connect(refresh);

// The overlay may already be up when the script (re)loads.
refresh();
