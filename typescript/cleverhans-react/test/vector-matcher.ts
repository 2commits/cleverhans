/**
 * Minimal matcher for client conformance vectors (spec/vectors/README.md):
 * subset object matching, exact-length arrays, and the `$bind`/`$ref`/
 * `$exact` directives. Mirrors the Rust matcher's semantics for the subset
 * the client vectors use.
 */

export type Bindings = Map<string, unknown>;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function directive(value: unknown): [string, unknown] | null {
  if (!isRecord(value)) {
    return null;
  }
  const keys = Object.keys(value);
  const key = keys[0];
  if (keys.length === 1 && key !== undefined && key.startsWith("$")) {
    return [key, value[key]];
  }
  return null;
}

/** Deep equality over JSON values. */
function deepEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Matches `expected` against `actual`, throwing a descriptive error on the
 * first mismatch.
 */
export function matchValue(
  expected: unknown,
  actual: unknown,
  bindings: Bindings,
  path = "root",
): void {
  const dir = directive(expected);
  if (dir) {
    const [key, arg] = dir;
    switch (key) {
      case "$bind":
        bindings.set(String(arg), actual);
        return;
      case "$ref": {
        const bound = bindings.get(String(arg));
        if (!deepEqual(bound, actual)) {
          throw new Error(`${path}: expected bound ${String(arg)}, got ${JSON.stringify(actual)}`);
        }
        return;
      }
      case "$exact":
        if (!deepEqual(arg, actual)) {
          throw new Error(
            `${path}: expected exactly ${JSON.stringify(arg)}, got ${JSON.stringify(actual)}`,
          );
        }
        return;
      case "$keys": {
        if (!isRecord(actual)) {
          throw new Error(`${path}: expected object, got ${JSON.stringify(actual)}`);
        }
        const want = [...(arg as string[])].sort();
        const got = Object.keys(actual).sort();
        if (!deepEqual(want, got)) {
          throw new Error(
            `${path}: expected keys ${JSON.stringify(want)}, got ${JSON.stringify(got)}`,
          );
        }
        return;
      }
      case "$absent":
        if (actual !== null && actual !== undefined) {
          throw new Error(`${path}: expected absent, got ${JSON.stringify(actual)}`);
        }
        return;
      default:
        throw new Error(`${path}: unknown directive ${key}`);
    }
  }
  if (isRecord(expected)) {
    if (!isRecord(actual)) {
      throw new Error(`${path}: expected object, got ${JSON.stringify(actual)}`);
    }
    for (const [key, want] of Object.entries(expected)) {
      const wantDir = directive(want);
      if (wantDir && wantDir[0] === "$absent") {
        const got = actual[key];
        if (got !== null && got !== undefined) {
          throw new Error(`${path}.${key}: expected absent, got ${JSON.stringify(got)}`);
        }
        continue;
      }
      if (!(key in actual)) {
        throw new Error(`${path}.${key}: missing`);
      }
      matchValue(want, actual[key], bindings, `${path}.${key}`);
    }
    return;
  }
  if (Array.isArray(expected)) {
    if (!Array.isArray(actual) || actual.length !== expected.length) {
      throw new Error(
        `${path}: expected ${expected.length} element(s), got ${JSON.stringify(actual)}`,
      );
    }
    expected.forEach((want, index) => {
      matchValue(want, actual[index], bindings, `${path}[${index}]`);
    });
    return;
  }
  if (expected !== actual) {
    throw new Error(
      `${path}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}
