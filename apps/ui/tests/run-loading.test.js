import assert from "node:assert/strict";
import { test } from "node:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { compileModule } from "svelte/compiler";
import { effect_root, flush } from "svelte/internal/client";
import ts from "typescript";
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
    headers: { "content-type": "application/grpc-web+proto" },
  });
}

test(
  "leaving a run before its initial log loads aborts the watch",
  { timeout: 5000 },
  async (t) => {
    const vite = await createServer({
      configFile: false,
      plugins: [
        {
          name: "resource-client-runes",
          transform(source, id) {
            if (!id.endsWith("/resource.svelte.ts")) return;
            const js = ts.transpileModule(source, {
              compilerOptions: {
                target: ts.ScriptTarget.ESNext,
                module: ts.ModuleKind.ESNext,
              },
            }).outputText;
            return compileModule(js, { filename: id, generate: "client" }).js;
          },
        },
      ],
      server: { middlewareMode: true, ws: false },
    });
    t.after(() => vite.close());
    const { resource } = await vite.ssrLoadModule(
      "/src/lib/resource.svelte.ts",
    );
    const { loadRun } = await vite.ssrLoadModule("/src/lib/data/run.ts");
    const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/run_pb.ts");
    let watchSignal;
    let started;
    const watching = new Promise((resolve) => {
      started = resolve;
    });
    t.mock.method(globalThis, "fetch", async (input, init) => {
      const method = String(input).split("/").at(-1);
      if (method === "GetRun")
        return response(pb.GetRunResponseSchema, {
          run: {
            publicId: "run",
            repoPublicId: "repo",
            trigger: pb.RunTrigger.WORKTREE,
            state: pb.RunState.RUNNING,
          },
        });
      if (method === "WatchRun") {
        watchSignal = init.signal;
        started();
        return new Promise((_, reject) => {
          init.signal.addEventListener(
            "abort",
            () => reject(init.signal.reason),
            { once: true },
          );
        });
      }
      // Empty protobuf responses suffice for the other page metadata.
      return response(pb.ListRunsResponseSchema, {});
    });
    let data;
    const leave = effect_root(() => {
      data = resource((signal) => loadRun("run", signal));
    });
    t.after(leave);
    flush();
    await watching;
    assert.equal(data.loading, true);
    assert.equal(watchSignal.aborted, false);
    leave();
    assert.equal(watchSignal.aborted, true);
  },
);
