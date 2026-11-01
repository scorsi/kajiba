import assert from "node:assert/strict";
import { test } from "node:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createServer } from "vite";

function response(schema, value) {
  const bytes = toBinary(schema, create(schema, value));
  const trailer = new TextEncoder().encode("grpc-status: 0\r\n");
  const frame = new Uint8Array(10 + bytes.length + trailer.length);
  new DataView(frame.buffer).setUint32(1, bytes.length);
  frame.set(bytes, 5);
  frame[5 + bytes.length] = 0x80;
  new DataView(frame.buffer).setUint32(6 + bytes.length, trailer.length);
  frame.set(trailer, 10 + bytes.length);
  return new Response(frame, {
    headers: {
      "content-type": "application/grpc-web+proto",
      "grpc-status": "0",
    },
  });
}

test("repository loads follow the current account and declaration without reloading the module", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { allRepos } = await vite.ssrLoadModule("/src/lib/loaders.ts");
  const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/repo_pb.ts");
  let viewer = "alice";
  let role = pb.Role.WRITE;
  const calls = [];
  t.mock.method(globalThis, "fetch", async (input, init) => {
    assert.equal(String(input).split("/").at(-1), "ListRepos");
    const request = fromBinary(
      pb.ListReposRequestSchema,
      new Uint8Array(init.body).subarray(5),
    );
    assert.equal(request.pageSize, 100);
    calls.push([viewer, request.pageToken]);
    return response(pb.ListReposResponseSchema, {
      repos: [
        {
          owner: viewer,
          name: request.pageToken ? "a" : "z",
          permissions: [{ user: viewer, role }],
        },
      ],
      nextPageToken: request.pageToken ? "" : "next",
    });
  });

  assert.deepEqual(
    (await allRepos()).map((r) => `${r.owner}/${r.name}`),
    ["alice/a", "alice/z"],
  );
  // A new session changes the RPC viewer while the loaded module stays alive.
  viewer = "bob";
  assert.deepEqual(
    (await allRepos()).map((r) => `${r.owner}/${r.name}`),
    ["bob/a", "bob/z"],
  );
  role = pb.Role.READ;
  assert.deepEqual(
    (await allRepos()).map((r) => r.permissions),
    [
      [{ user: "bob", role: pb.Role.READ }],
      [{ user: "bob", role: pb.Role.READ }],
    ],
  );
  assert.deepEqual(calls, [
    ["alice", ""],
    ["alice", "next"],
    ["bob", ""],
    ["bob", "next"],
    ["bob", ""],
    ["bob", "next"],
  ]);
});

test("recent runs reject a partial failure and can be retried", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { recentRuns } = await vite.ssrLoadModule("/src/lib/loaders.ts");
  const repos = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/repo_pb.ts");
  const runs = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/run_pb.ts");
  let failing = false;
  t.mock.method(globalThis, "fetch", async (input, init) => {
    if (String(input).endsWith("/ListRepos")) {
      return response(repos.ListReposResponseSchema, {
        repos: ["a", "b"].map((name) => ({ owner: "alice", name })),
      });
    }
    assert.ok(String(input).endsWith("/ListRuns"));
    const request = fromBinary(
      runs.ListRunsRequestSchema,
      new Uint8Array(init.body).subarray(5),
    );
    assert.equal(request.pageSize, 3);
    if (failing && request.repo === "alice/b") {
      throw new Error("temporary ListRuns failure");
    }
    const newer = request.repo === "alice/b";
    return response(runs.ListRunsResponseSchema, {
      runs: [
        {
          publicId: request.repo,
          state: runs.RunState.RUNNING,
          createdAt: { seconds: newer ? 2n : 1n },
        },
      ],
    });
  });

  const snapshot = await recentRuns(3);
  assert.deepEqual(
    snapshot.map((r) => r.id),
    ["alice/b", "alice/a"],
  );
  failing = true;
  await assert.rejects(recentRuns(3), /temporary ListRuns failure/);
  failing = false;
  assert.deepEqual(await recentRuns(3), snapshot);
});
