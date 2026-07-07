/**
 * Runs every agent/binding conformance vector through the napi binding,
 * with handlers/dry-runs/authz as JS callbacks — the bridge is under test.
 */

import { describe, it } from "vitest";

import { loadDir, runVector } from "./vector-runner";

const fixtures = new Map(loadDir("fixtures").map((fixture) => [fixture.name, fixture]));
const vectors = loadDir("cases");

describe("conformance vectors", () => {
  for (const vector of vectors) {
    it(String(vector.name), async () => {
      const fixture = fixtures.get(vector.fixture);
      if (!fixture) {
        throw new Error(`unknown fixture ${String(vector.fixture)}`);
      }
      await runVector(fixture, vector);
    });
  }
});
