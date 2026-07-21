/**
 * Node port of the conformance runner (spec/vectors/README.md). Handlers,
 * dry-runs, and authz are JS callbacks so the vectors exercise the napi
 * bridge itself; slot content goes through the declarative table the
 * binding supports.
 */

import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";

import { Agent, type JsonObject, Rejected, type ServerEvent, type SlotTable } from "../src/index";

const VECTORS = path.join(__dirname, "../../../spec/vectors");

export function loadDir(sub: string): JsonObject[] {
  const dir = path.join(VECTORS, sub);
  return readdirSync(dir)
    .filter((file) => file.endsWith(".json"))
    .sort()
    .map((file) => JSON.parse(readFileSync(path.join(dir, file), "utf8")) as JsonObject);
}

// --- matcher (README "Matching semantics") -------------------------------

type Bindings = Map<string, unknown>;

function directive(value: unknown): [string, unknown] | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  const keys = Object.keys(value);
  const key = keys[0];
  if (keys.length === 1 && key !== undefined && key.startsWith("$")) {
    return [key, (value as Record<string, unknown>)[key]];
  }
  return null;
}

function deepEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function substitute(payload: unknown, bindings: Bindings): unknown {
  const dir = directive(payload);
  if (dir && dir[0] === "$ref") {
    const name = String(dir[1]);
    if (!bindings.has(name)) {
      throw new Error(`$ref to unbound name \`${name}\``);
    }
    return bindings.get(name);
  }
  if (Array.isArray(payload)) {
    return payload.map((value) => substitute(value, bindings));
  }
  if (typeof payload === "object" && payload !== null) {
    return Object.fromEntries(
      Object.entries(payload).map(([key, value]) => [key, substitute(value, bindings)]),
    );
  }
  return payload;
}

export function matchValue(
  expected: unknown,
  actual: unknown,
  bindings: Bindings,
  at: string,
): void {
  const dir = directive(expected);
  if (dir) {
    const [key, arg] = dir;
    switch (key) {
      case "$bind":
        bindings.set(String(arg), actual);
        return;
      case "$ref":
        if (!deepEqual(bindings.get(String(arg)), actual)) {
          throw new Error(`${at}: expected bound ${String(arg)}, got ${JSON.stringify(actual)}`);
        }
        return;
      case "$exact":
        if (!deepEqual(arg, actual)) {
          throw new Error(`${at}: expected exactly ${JSON.stringify(arg)}, got ${JSON.stringify(actual)}`);
        }
        return;
      case "$keys": {
        const want = [...(arg as string[])].sort();
        const got = Object.keys(actual as object).sort();
        if (!deepEqual(want, got)) {
          throw new Error(`${at}: expected keys ${JSON.stringify(want)}, got ${JSON.stringify(got)}`);
        }
        return;
      }
      case "$absent":
        if (actual !== null && actual !== undefined) {
          throw new Error(`${at}: expected absent, got ${JSON.stringify(actual)}`);
        }
        return;
      default:
        throw new Error(`${at}: unknown directive ${key}`);
    }
  }
  if (Array.isArray(expected)) {
    if (!Array.isArray(actual) || actual.length !== expected.length) {
      throw new Error(`${at}: expected ${expected.length} element(s), got ${JSON.stringify(actual)}`);
    }
    expected.forEach((want, index) => matchValue(want, actual[index], bindings, `${at}[${index}]`));
    return;
  }
  if (typeof expected === "object" && expected !== null) {
    if (typeof actual !== "object" || actual === null) {
      throw new Error(`${at}: expected object, got ${JSON.stringify(actual)}`);
    }
    for (const [key, want] of Object.entries(expected)) {
      const wantDir = directive(want);
      const got = (actual as Record<string, unknown>)[key];
      if (wantDir && wantDir[0] === "$absent") {
        if (got !== null && got !== undefined) {
          throw new Error(`${at}.${key}: expected absent, got ${JSON.stringify(got)}`);
        }
        continue;
      }
      if (!(key in (actual as object))) {
        throw new Error(`${at}.${key}: missing in ${JSON.stringify(actual)}`);
      }
      matchValue(want, got, bindings, `${at}.${key}`);
    }
    return;
  }
  if (expected !== actual) {
    throw new Error(`${at}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

export function matchEvents(expected: unknown[], actual: unknown[], bindings: Bindings): void {
  if (expected.length !== actual.length) {
    throw new Error(
      `expected ${expected.length} event(s), got ${actual.length}: ${JSON.stringify(actual)}`,
    );
  }
  expected.forEach((want, index) => matchValue(want, actual[index], bindings, `event[${index}]`));
}

// --- scripted seams as JS callbacks --------------------------------------

type Script = Record<string, any>;

function behaviorAt(script: Script, call: number): any {
  if ("default" in script) {
    return script.default;
  }
  return call < script.sequence.length ? script.sequence[call] : script.then;
}

export function buildAgent(fixture: Script, vector: Script): { agent: Agent; executions: JsonObject[] } {
  const executions: JsonObject[] = [];
  const handlers: Record<string, any> = {};
  const dryRuns: Record<string, any> = {};
  const slotBuilders: Record<string, SlotTable> = {};

  for (const action of fixture.registry.actions) {
    const script = fixture.scripts[action.id];
    handlers[action.id] = async (params: JsonObject) => {
      executions.push({ action_id: action.id, params });
      if ("fail" in script.handler) {
        throw new Rejected(script.handler.fail);
      }
      return script.handler.return;
    };
    if (script.dry_run) {
      let calls = 0;
      dryRuns[action.id] = async () => {
        const behavior =
          "sequence" in script.dry_run ? behaviorAt(script.dry_run, calls++) : script.dry_run;
        if ("fail" in behavior) {
          throw new Rejected(behavior.fail);
        }
        return behavior.preview;
      };
    }
    if (script.slots) {
      slotBuilders[action.id] = script.slots;
    }
  }

  const authzScript = vector.authz ?? { default: "allow" };
  let authzCalls = 0;
  const authorize = async () => {
    const behavior = behaviorAt(authzScript, authzCalls++);
    return behavior === "allow" ? null : behavior.deny;
  };

  const agent = new Agent({
    registry: fixture.registry,
    handlers,
    dryRuns,
    slotBuilders,
    authorize,
    llm: { provider: "scripted", script: vector.llm ?? [] },
  });
  return { agent, executions };
}

// --- the runner -----------------------------------------------------------

function normalize(events: ServerEvent[], vector: Script): ServerEvent[] {
  return events.filter((event) => {
    if ((vector.ignore_types ?? []).includes(event.type)) {
      return false;
    }
    return vector.keep_deltas || !(event.type === "chat_message" && event.done === false);
  });
}

export async function runVector(fixture: Script, vector: Script): Promise<void> {
  const { agent, executions } = buildAgent(fixture, vector);
  const session = agent.session({ vector: vector.name });
  const bindings: Bindings = new Map();

  if (vector.layer === "agent") {
    let buffer: ServerEvent[] = [];
    for (const step of vector.steps) {
      if ("send" in step) {
        buffer.push(...(await session.handleCollect(substitute(step.send, bindings) as JsonObject)));
      } else {
        const actual = normalize(buffer, vector);
        buffer = [];
        matchEvents(step.expect, actual, bindings);
      }
    }
    if (normalize(buffer, vector).length > 0) {
      throw new Error("trailing events after the last expect");
    }
  } else {
    const actual: ServerEvent[] = [];
    for (const frame of vector.frames) {
      if (session.closed) {
        break; // the stream closed; a transport would read no further
      }
      const raw = "raw" in frame ? frame.raw : JSON.stringify(frame.json);
      actual.push(...(await session.handleCollect(raw)));
    }
    const expectClose = vector.expect_close ?? false;
    if (session.closed !== expectClose) {
      throw new Error(
        `expect_close: expected closed = ${expectClose}, session closed = ${session.closed}`,
      );
    }
    matchEvents(vector.expect, normalize(actual, vector), bindings);
  }

  if (vector.executions != null && !deepEqual(executions, vector.executions)) {
    throw new Error(
      `executions diverge: expected ${JSON.stringify(vector.executions)}, got ${JSON.stringify(executions)}`,
    );
  }
}
