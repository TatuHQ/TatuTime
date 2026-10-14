// Shared condition-wait primitives for the jsdom GUI harness (#265).
//
// Every harness interaction - a click, a form submit, a filter `change` - ends in
// an asynchronous fetch plus a re-render. The harness used to sleep a fixed
// timer tick and then assert, so it only passed when the runner happened to be
// fast enough. That is the documented root cause of the recurring `gui-test`
// flake family (the "self-hosted runner lost communication"/early-assert
// symptoms and the `copy-previous-day` timing failure).
//
// `waitFor` polls the condition the next assertion actually depends on (element
// present, text updated, view rendered, dialog open, list settled) and fails
// loudly when it never holds, so a check fails on *state*, never on wall-clock
// luck. A timeout is always a real failure - it is never swallowed.
//
// The poll interval is a short timer, but it is not a delay the assertion races:
// the loop exits as soon as the predicate holds, and only the predicate decides
// when that is. `gui-test/condition-waits.test.mjs` guards the harness files
// against regressing to fixed-duration waits.

export const DEFAULT_TIMEOUT_MS = 5000;
export const DEFAULT_INTERVAL_MS = 10;

/**
 * Resolve as soon as `predicate()` is truthy; reject with a descriptive error
 * once `timeoutMs` has elapsed. The predicate is polled (never spun) every
 * `intervalMs`; it may be synchronous or asynchronous, and an error it throws is
 * treated as "not yet" and reported if the wait finally times out.
 *
 * @param {() => unknown} predicate condition to wait for
 * @param {{ timeoutMs?: number, intervalMs?: number, message?: string }} [options]
 * @returns {Promise<void>}
 */
export function waitFor(predicate, options = {}) {
  const {
    timeoutMs = DEFAULT_TIMEOUT_MS,
    intervalMs = DEFAULT_INTERVAL_MS,
    message = 'the condition to hold',
  } = options;
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    let lastError = null;
    const poll = async () => {
      let held = false;
      try {
        held = Boolean(await predicate());
      } catch (error) {
        lastError = error;
      }
      if (held) {
        resolve();
        return;
      }
      if (Date.now() >= deadline) {
        const detail = lastError ? `; the predicate last threw: ${lastError.message}` : '';
        reject(new Error(`waitFor: timed out after ${timeoutMs}ms waiting for ${message}${detail}`));
        return;
      }
      setTimeout(poll, intervalMs);
    };
    poll();
  });
}

/**
 * Yield one turn of the event loop so work queued behind an `await` - the
 * re-render that follows a fetch - gets to run before the harness looks again.
 */
export const yieldToEventLoop = () => new Promise((resolve) => {
  setTimeout(resolve, 0);
});
