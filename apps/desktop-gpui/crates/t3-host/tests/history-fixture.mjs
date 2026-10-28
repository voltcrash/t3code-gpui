// Executed with node --eval from apps/server so its dependencies resolve there.
// Persist stopped conversations through the real stores without starting provider turns.
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Sqlite from "./src/persistence/Sqlite.ts";
import * as Events from "./src/orchestration-v2/EventStore.ts";
import * as Projections from "./src/orchestration-v2/ProjectionStore.ts";
import * as Projects from "./src/orchestration-v2/ProjectStore.ts";

const dbPath = process.argv[1];
const workspaceRoot = process.argv[2];
const threadId = "history-fixture";
const projectId = "history-project";
const now = DateTime.makeUnsafe("2026-10-24T10:00:00.000Z");
const layer = Layer.mergeAll(Events.layer, Projections.layer, Projects.layer).pipe(
  Layer.provide(Sqlite.layerFromPath(dbPath)),
  Layer.provide(NodeServices.layer),
);
await Effect.runPromise(
  Effect.gen(function* () {
    const projects = yield* Projects.ProjectStoreV2;
    yield* projects.apply({
      sequence: 0,
      eventId: "history-project-created",
      aggregateKind: "project",
      aggregateId: projectId,
      occurredAt: DateTime.formatIso(now),
      commandId: null,
      causationEventId: null,
      correlationId: null,
      metadata: {},
      type: "project.created",
      payload: {
        projectId,
        title: "History fixture",
        workspaceRoot,
        defaultModelSelection: null,
        scripts: [],
        createdAt: DateTime.formatIso(now),
        updatedAt: DateTime.formatIso(now),
      },
    });
    const events = [
      {
        id: "history-thread-created",
        type: "thread.created",
        threadId,
        providerInstanceId: "codex",
        occurredAt: now,
        payload: {
          id: threadId,
          projectId,
          createdBy: "user",
          creationSource: "web",
          title: "Long conversation",
          providerInstanceId: "codex",
          modelSelection: { instanceId: "codex", model: "gpt-6-astra" },
          runtimeMode: "approval-required",
          interactionMode: "default",
          branch: null,
          worktreePath: null,
          activeProviderThreadId: null,
          lineage: { parentThreadId: null, relationshipToParent: null, rootThreadId: threadId },
          forkedFrom: null,
          createdAt: now,
          updatedAt: now,
          archivedAt: null,
          settledOverride: null,
          settledAt: null,
          lastVisitedAt: null,
          deletedAt: null,
        },
      },
    ];
    for (let turn = 1; turn <= 35; turn++) {
      for (const type of ["user_message", "assistant_message"]) {
        const id = `${type}-${turn}`;
        events.push({
          id: `event-${id}`,
          type: "turn-item.updated",
          threadId,
          occurredAt: now,
          payload: {
            id,
            threadId,
            type,
            messageId: id,
            runId: null,
            nodeId: null,
            providerThreadId: null,
            providerTurnId: null,
            nativeItemRef: null,
            parentItemId: null,
            ordinal: turn * 2 - (type === "user_message" ? 1 : 0),
            status: "completed",
            title: null,
            text:
              type === "user_message"
                ? `Question ${turn}`
                : turn === 1
                  ? "Answer 1. First **needle** and second needle.\n\n[Reference](https://hidden.example/needle)"
                  : `Answer ${turn}. This message is stored on the existing Node server.`,
            startedAt: now,
            completedAt: now,
            updatedAt: now,
            ...(type === "user_message"
              ? {
                  inputIntent: "turn_start",
                  attachments: [],
                  createdBy: "user",
                  creationSource: "web",
                }
              : { streaming: false }),
          },
        });
        const item = events.at(-1).payload;
        events.push({
          id: `message-${id}`,
          type: "message.updated",
          threadId,
          occurredAt: now,
          payload: {
            id,
            threadId,
            runId: null,
            nodeId: null,
            role: type === "user_message" ? "user" : "assistant",
            text: item.text,
            attachments: [],
            streaming: false,
            createdBy: type === "user_message" ? "user" : "agent",
            creationSource: type === "user_message" ? "web" : "provider",
            createdAt: now,
            updatedAt: now,
          },
        });
      }
    }
    const outputThread = "output-fixture";
    events.push({
      ...events[0],
      id: "output-thread-created",
      threadId: outputThread,
      payload: { ...events[0].payload, id: outputThread, title: "Command output fixture" },
    });
    for (const [id, input, output, exitCode] of [
      ["output-command", "printf 'café\\n'", "café\n" + "output line\n".repeat(400), 0],
      ["empty-command", "true", "", 0],
      ["failed-command", "false", '{"stdout":"","stderr":"command failed","interrupted":false}', 1],
    ]) {
      events.push({
        id: `event-${id}`,
        type: "turn-item.updated",
        threadId: outputThread,
        occurredAt: now,
        payload: {
          id,
          threadId: outputThread,
          type: "command_execution",
          ordinal: events.length,
          runId: null,
          nodeId: null,
          providerThreadId: null,
          providerTurnId: null,
          nativeItemRef: null,
          parentItemId: null,
          status: "completed",
          title: null,
          input,
          output,
          exitCode,
          startedAt: now,
          completedAt: now,
          updatedAt: now,
        },
      });
    }
    const store = yield* Events.EventStoreV2;
    const projection = yield* Projections.ProjectionStoreV2;
    yield* store.append({ events });
    yield* Effect.forEach(events, projection.apply, { discard: true });
  }).pipe(Effect.provide(layer)),
);
