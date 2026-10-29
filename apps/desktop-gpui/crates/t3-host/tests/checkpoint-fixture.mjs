// Isolated checkpoint refs and persisted events, with no provider processes.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeCrypto from "@effect/platform-node/NodeCrypto";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Sqlite from "./src/persistence/Sqlite.ts";
import * as Events from "./src/orchestration-v2/EventStore.ts";
import * as Projections from "./src/orchestration-v2/ProjectionStore.ts";
import { checkpointRefForScopeOrdinal } from "./src/orchestration-v2/CheckpointService.ts";

const dbPath = process.argv[1];
const cwd = process.argv[2];
const threadId = "history-fixture";
const scopeId = "fixture-checkpoint-scope";
const nodeId = "fixture-checkpoint-node";
const now = DateTime.makeUnsafe("2026-10-26T10:00:00.000Z");
const env = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_AUTHOR_NAME: "T3 fixture",
  GIT_AUTHOR_EMAIL: "fixture@example.invalid",
  GIT_COMMITTER_NAME: "T3 fixture",
  GIT_COMMITTER_EMAIL: "fixture@example.invalid",
};
NodeFS.mkdirSync(cwd, { recursive: true });
const git = (...args) =>
  NodeChildProcess.execFileSync("git", ["-c", "commit.gpgsign=false", ...args], {
    cwd,
    env,
    encoding: "utf8",
  }).trim();
git("init", "--initial-branch=fixture");
NodeFS.writeFileSync(`${cwd}/app.rs`, 'fn main() {\n    println!("before checkpoint");\n}\n');
NodeFS.writeFileSync(`${cwd}/spacing.rs`, "let value=1;\n");
git("add", ".");
git("commit", "-m", "Checkpoint baseline");
const refs = [];
const commits = [];
for (let ordinal = 0; ordinal <= 2; ordinal++) {
  if (ordinal === 1) {
    NodeFS.writeFileSync(`${cwd}/app.rs`, 'fn main() {\n    println!("first checkpoint");\n}\n');
    NodeFS.writeFileSync(`${cwd}/helper.rs`, "pub fn helper() {}\n");
  } else if (ordinal === 2) {
    NodeFS.writeFileSync(`${cwd}/app.rs`, 'fn main() {\n    println!("second checkpoint");\n}\n');
    NodeFS.writeFileSync(`${cwd}/spacing.rs`, "let value = 1;\n");
    NodeFS.renameSync(`${cwd}/helper.rs`, `${cwd}/support.rs`);
    NodeFS.writeFileSync(`${cwd}/asset.bin`, Buffer.from([0, 1, 2, 3]));
  }
  git("add", ".");
  const tree = git("write-tree");
  const ref = await Effect.runPromise(
    checkpointRefForScopeOrdinal({ scopeId, ordinalWithinScope: ordinal }).pipe(
      Effect.provide(NodeCrypto.layer),
    ),
  );
  const commit = git("commit-tree", tree, "-p", "HEAD", "-m", `t3 checkpoint ref=${ref}`);
  git("update-ref", ref, commit);
  refs.push(ref);
  commits.push(commit);
}
// A turn comparison must never include the current, uncaptured working tree.
NodeFS.writeFileSync(`${cwd}/app.rs`, 'fn main() { println!("live uncaptured edit"); }\n');
NodeFS.writeFileSync(`${cwd}/refs.json`, JSON.stringify({ refs, commits }));

const events = [];
for (let ordinal = 1; ordinal <= 2; ordinal++) {
  const runId = `checkpoint-run-${ordinal}`;
  events.push({
    id: `checkpoint-run-event-${ordinal}`,
    type: "run.updated",
    threadId,
    runId,
    occurredAt: now,
    payload: {
      id: runId,
      threadId,
      ordinal,
      providerInstanceId: "codex",
      modelSelection: { instanceId: "codex", model: "gpt-6-astra" },
      providerThreadId: null,
      userMessageId: `user_message-${ordinal + 33}`,
      rootNodeId: null,
      activeAttemptId: null,
      status: "completed",
      requestedAt: now,
      startedAt: now,
      completedAt: now,
      checkpointId: `fixture-checkpoint-${ordinal}`,
      contextHandoffId: null,
    },
  });
}
events.push({
  id: "checkpoint-scope-event",
  type: "checkpoint-scope.created",
  threadId,
  occurredAt: now,
  payload: {
    id: scopeId,
    threadId,
    runId: "checkpoint-run-1",
    nodeId,
    parentScopeId: null,
    providerThreadId: null,
    kind: "root_run",
    ordinalWithinParent: 0,
    advancesAppRunCount: true,
    cwd,
    createdAt: now,
  },
});
for (let ordinal = 0; ordinal <= 2; ordinal++) {
  events.push({
    id: `checkpoint-event-${ordinal}`,
    type: "checkpoint.captured",
    threadId,
    occurredAt: now,
    payload: {
      id: `fixture-checkpoint-${ordinal}`,
      threadId,
      scopeId,
      nodeId,
      runId: ordinal === 0 ? null : `checkpoint-run-${ordinal}`,
      parentCheckpointId: ordinal === 0 ? null : `fixture-checkpoint-${ordinal - 1}`,
      ordinalWithinScope: ordinal,
      appRunOrdinal: ordinal === 0 ? null : ordinal,
      ref: refs[ordinal],
      status: "ready",
      files: [],
      capturedAt: now,
    },
  });
}
const layer = Layer.mergeAll(Events.layer, Projections.layer).pipe(
  Layer.provide(Sqlite.layerFromPath(dbPath)),
  Layer.provide(NodeServices.layer),
);
await Effect.runPromise(
  Effect.gen(function* () {
    const store = yield* Events.EventStoreV2;
    const projection = yield* Projections.ProjectionStoreV2;
    const current = yield* projection.getThreadProjection(threadId);
    for (let ordinal = 1; ordinal <= 2; ordinal++) {
      for (const type of ["user_message", "assistant_message"]) {
        const item = current.turnItems.find((item) => item.id === `${type}-${ordinal + 33}`);
        events.push({
          id: `checkpoint-message-${type}-${ordinal}`,
          type: "turn-item.updated",
          threadId,
          occurredAt: now,
          payload: { ...item, runId: `checkpoint-run-${ordinal}` },
        });
      }
    }
    yield* store.append({ events });
    yield* Effect.forEach(events, projection.apply, { discard: true });
  }).pipe(Effect.provide(layer)),
);
