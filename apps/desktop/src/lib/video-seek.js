/** @param {number} toleranceMs */
export function createVideoSeekCoordinator(toleranceMs = 0.5) {
  /** @type {number | null} */
  let desiredMs = null;
  let inFlight = false;

  return {
    /** @param {number} targetMs */
    request(targetMs) {
      desiredMs = targetMs;
      if (inFlight) return null;
      inFlight = true;
      return targetMs;
    },

    /** @param {number} actualMs */
    settle(actualMs) {
      inFlight = false;
      if (desiredMs !== null && Math.abs(desiredMs - actualMs) > toleranceMs) {
        inFlight = true;
        return desiredMs;
      }
      desiredMs = null;
      return null;
    },

    reset() {
      desiredMs = null;
      inFlight = false;
    },

    get pending() {
      return inFlight;
    },
  };
}
