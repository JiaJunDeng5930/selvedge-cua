// Position is sampled before native dispatch. The host owns time and button
// state during dispatch, so callbacks cannot interrupt or alter a drag.
((dispatch, offsets) => async function pointer(options) {
    const { windowId, position, durationMs = 0, intervalMs = 16, buttons = [], hold = false, operation } = options;
    if (position === undefined && durationMs !== 0) {
        throw new TypeError("pointer position is required when durationMs is positive");
    }
    if (position !== undefined && typeof position !== "function") {
        throw new TypeError("pointer position must be a function");
    }

    const samples = [];
    const sample = (atMs) => {
        const { x, y } = position(atMs);
        if (!Number.isFinite(x) || !Number.isFinite(y)) {
            throw new TypeError("pointer position must return finite x and y coordinates");
        }
        samples.push({ atMs, x, y });
    };
    if (position !== undefined) {
        for (const atMs of offsets(durationMs, intervalMs)) {
            sample(atMs);
        }
    }
    return dispatch({ windowId, samples, buttons, hold, operation });
})
