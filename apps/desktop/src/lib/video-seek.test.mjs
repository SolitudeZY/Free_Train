import assert from "node:assert/strict";
import test from "node:test";

import { createVideoSeekCoordinator } from "./video-seek.js";

test("coalesces repeated frame steps while a seek is active", () => {
  const coordinator = createVideoSeekCoordinator();

  assert.equal(coordinator.request(16.667), 16.667);
  assert.equal(coordinator.request(33.333), null);
  assert.equal(coordinator.request(50), null);
  assert.equal(coordinator.request(66.667), null);
  assert.equal(coordinator.settle(16.667), 66.667);
  assert.equal(coordinator.settle(66.667), null);
  assert.equal(coordinator.pending, false);
});

test("reset drops a stale seek when the active source changes", () => {
  const coordinator = createVideoSeekCoordinator();

  assert.equal(coordinator.request(1_000), 1_000);
  coordinator.reset();
  assert.equal(coordinator.pending, false);
  assert.equal(coordinator.request(2_000), 2_000);
});
