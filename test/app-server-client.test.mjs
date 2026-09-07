import assert from "node:assert/strict";
import test from "node:test";
import {
  sanitizeAccountResult,
  sanitizeRateLimitsResult,
} from "../src/core/app-server-client.mjs";

test("sanitizeAccountResult redacts email and keeps safe account metadata", () => {
  assert.deepEqual(
    sanitizeAccountResult({
      account: { type: "chatgpt", email: "alice@example.com", planType: "plus" },
      requiresOpenaiAuth: true,
    }),
    {
      account: { type: "chatgpt", email: "a***@example.com", planType: "plus" },
      requiresOpenaiAuth: true,
    },
  );
});

test("sanitizeRateLimitsResult calculates remaining percentage and reset time", () => {
  const result = sanitizeRateLimitsResult({
    ordinaryUsageAllowed: true,
    rateLimits: {
      primary: {
        usedPercent: 25,
        windowDurationMins: 300,
        resetsAt: 1_730_947_200,
      },
      secondary: null,
      rateLimitReachedType: null,
    },
  });

  assert.equal(result.rateLimits.primary.remainingPercent, 75);
  assert.equal(result.rateLimits.primary.windowDurationMins, 300);
  assert.equal(result.rateLimits.primary.resetsAtIso, "2024-11-07T02:40:00.000Z");
  assert.equal(result.rateLimits.secondary, null);
});

test("sanitizeRateLimitsResult supports absent and additional limits", () => {
  const result = sanitizeRateLimitsResult({
    additionalRateLimits: [
      {
        limitName: "spark",
        modelSlug: "gpt-spark",
        rateLimits: {
          primary: { usedPercent: 120, windowDurationMins: 60, resetsAt: 100 },
        },
      },
    ],
  });

  assert.equal(result.ordinaryUsageAllowed, null);
  assert.equal(result.additionalRateLimits[0].rateLimits.primary.remainingPercent, 0);
});
