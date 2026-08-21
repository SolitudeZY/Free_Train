import assert from "node:assert/strict";
import test from "node:test";

import { calculateReviewWindow } from "./review-virtual.js";

test("review window bounds DOM work for one hundred thousand items", () => {
  const window = calculateReviewWindow({
    itemCount: 100_000,
    viewportWidth: 1_200,
    viewportHeight: 760,
    scrollTop: 0,
  });
  assert.equal(window.columns, 6);
  assert.equal(window.start, 0);
  assert.ok(window.end <= 48);
  assert.ok(window.totalHeight > 3_000_000);
});

test("review window reaches the final item without rendering the full collection", () => {
  const initial = calculateReviewWindow({
    itemCount: 100_000,
    viewportWidth: 1_200,
    viewportHeight: 760,
    scrollTop: 0,
  });
  const final = calculateReviewWindow({
    itemCount: 100_000,
    viewportWidth: 1_200,
    viewportHeight: 760,
    scrollTop: initial.totalHeight,
  });
  assert.equal(final.end, 100_000);
  assert.ok(final.start > 99_900);
  assert.ok(final.end - final.start <= 48);
});

test("review window adapts its columns to a narrow viewport", () => {
  const window = calculateReviewWindow({
    itemCount: 100,
    viewportWidth: 380,
    viewportHeight: 600,
    scrollTop: 380,
  });
  assert.equal(window.columns, 2);
  assert.equal(window.start, 0);
  assert.ok(window.end < 30);
});
