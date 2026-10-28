// Seed a completed plan without starting a provider or changing server code.
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Sqlite from "./src/persistence/Sqlite.ts";
import * as Events from "./src/orchestration-v2/EventStore.ts";
import * as Projections from "./src/orchestration-v2/ProjectionStore.ts";

const threadId = "planning-fixture";
const runId = "planning-run";
const now = DateTime.makeUnsafe("2026-10-26T12:00:00.000Z");
const markdown =
  "# Native planning\n\n1. Add the native control.\n2. Verify the existing server protocol.\n3. Keep the Node server unchanged.";
const layer = Layer.mergeAll(Events.layer, Projections.layer).pipe(
  Layer.provide(Sqlite.layerFromPath(process.argv[1])),
  Layer.provide(NodeServices.layer),
);
await Effect.runPromise(
  Effect.gen(function* () {
    const projection = yield* Projections.ProjectionStoreV2;
    const original = yield* projection.getThread("history-fixture");
    const events = [
      {
        id: "planning-thread-created",
        type: "thread.created",
        threadId,
        occurredAt: now,
        payload: {
          ...original,
          id: threadId,
          title: "Plan workflow",
          interactionMode: "plan",
          activeProviderThreadId: null,
          createdAt: now,
          updatedAt: now,
          lineage: { parentThreadId: null, relationshipToParent: null, rootThreadId: threadId },
          forkedFrom: null,
        },
      },
      {
        id: "planning-run-created",
        type: "run.updated",
        threadId,
        runId,
        occurredAt: now,
        payload: {
          id: runId,
          threadId,
          ordinal: 1,
          providerInstanceId: "codex",
          modelSelection: { instanceId: "codex", model: "gpt-6-astra" },
          providerThreadId: null,
          userMessageId: "planning-user",
          rootNodeId: null,
          activeAttemptId: null,
          status: "completed",
          requestedAt: now,
          startedAt: now,
          completedAt: now,
          checkpointId: null,
          contextHandoffId: null,
        },
      },
      {
        id: "planning-artifact-created",
        type: "plan.updated",
        threadId,
        runId,
        occurredAt: now,
        payload: {
          id: "planning-plan",
          threadId,
          runId,
          nodeId: "planning-node",
          kind: "proposed_plan",
          status: "active",
          markdown,
        },
      },
    ];
    for (const type of ["user_message", "proposed_plan"]) {
      const user = type === "user_message";
      events.push({
        id: `planning-item-${type}`,
        type: "turn-item.updated",
        threadId,
        runId,
        occurredAt: now,
        payload: {
          id: user ? "planning-user-item" : "planning-plan-item",
          threadId,
          runId,
          nodeId: null,
          providerThreadId: null,
          providerTurnId: null,
          nativeItemRef: null,
          parentItemId: null,
          ordinal: user ? 1 : 2,
          status: "completed",
          title: null,
          type,
          startedAt: now,
          completedAt: now,
          updatedAt: now,
          ...(user
            ? {
                messageId: "planning-user",
                text: "Plan the native workflow.",
                inputIntent: "turn_start",
                attachments: [],
                createdBy: "user",
                creationSource: "web",
              }
            : { planId: "planning-plan", markdown, streaming: false }),
        },
      });
    }
    const tasksThread = "tasks-fixture";
    const tasksRun = "tasks-run";
    const providerThread = "tasks-provider";
    const steps = [
      {
        id: "inspect",
        text: "Inspect the existing server protocol",
        status: "completed",
        durationMs: 1234,
      },
      { id: "implement", text: "Implement native task progress", status: "running" },
      { id: "verify", text: "Verify narrow layouts and preserve the draft", status: "pending" },
    ];
    events.push(
      {
        ...events[0],
        id: "tasks-thread-created",
        threadId: tasksThread,
        payload: {
          ...events[0].payload,
          id: tasksThread,
          title: "Task progress fixture",
          interactionMode: "default",
          providerInstanceId: "claudeAgent",
          activeProviderThreadId: providerThread,
          modelSelection: { instanceId: "claudeAgent", model: "claude-sonnet-4-6" },
        },
      },
      {
        id: "tasks-provider-updated",
        type: "provider-thread.updated",
        threadId: tasksThread,
        occurredAt: now,
        payload: {
          id: providerThread,
          driver: "claudeAgent",
          providerInstanceId: "claudeAgent",
          providerSessionId: null,
          appThreadId: tasksThread,
          ownerNodeId: null,
          nativeThreadRef: null,
          nativeConversationHeadRef: null,
          status: "idle",
          firstRunOrdinal: 1,
          lastRunOrdinal: 1,
          handoffIds: [],
          forkedFrom: null,
          createdAt: now,
          updatedAt: now,
        },
      },
      {
        ...events[1],
        id: "tasks-run-updated",
        threadId: tasksThread,
        runId: tasksRun,
        payload: {
          ...events[1].payload,
          id: tasksRun,
          threadId: tasksThread,
          providerInstanceId: "claudeAgent",
          providerThreadId: providerThread,
          modelSelection: { instanceId: "claudeAgent", model: "claude-sonnet-4-6" },
          userMessageId: "tasks-user",
        },
      },
      {
        id: "tasks-plan-updated",
        type: "plan.updated",
        threadId: tasksThread,
        runId: tasksRun,
        occurredAt: now,
        payload: {
          id: "tasks-plan",
          threadId: tasksThread,
          runId: tasksRun,
          nodeId: "tasks-node",
          kind: "todo_list",
          status: "active",
          steps,
        },
      },
    );
    const store = yield* Events.EventStoreV2;
    yield* store.append({ events });
    yield* Effect.forEach(events, projection.apply, { discard: true });
  }).pipe(Effect.provide(layer)),
);
