import assert from "node:assert/strict";
import { test } from "node:test";
import { create, toBinary } from "@bufbuild/protobuf";
import { ConnectError } from "@connectrpc/connect";
import { createServer } from "vite";

function response(schema, events) {
  const frames = events.map((event) => {
    const bytes = toBinary(schema, create(schema, { event }));
    const frame = new Uint8Array(5 + bytes.length);
    new DataView(frame.buffer).setUint32(1, bytes.length);
    frame.set(bytes, 5);
    return frame;
  });
  const trailer = new TextEncoder().encode("grpc-status: 14\r\n");
  const frame = new Uint8Array(5 + trailer.length);
  frame[0] = 0x80;
  new DataView(frame.buffer).setUint32(1, trailer.length);
  frame.set(trailer, 5);
  return new Response(new Blob([...frames, frame]), {
    headers: { "content-type": "application/grpc-web+proto" },
  });
}

for (const progress of [false, true]) {
  test(`watch retries stop without new bytes (progress=${progress})`, async (t) => {
    const vite = await createServer({
      configFile: false,
      server: { middlewareMode: true, ws: false },
    });
    t.after(() => vite.close());
    const { watchRun, newRunLog, Code } =
      await vite.ssrLoadModule("/src/lib/api.ts");
    const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/run_pb.ts");
    const log = newRunLog();
    log.section("attempt", {});
    log.bytes(0, new TextEncoder().encode("a"));
    let calls = 0;
    t.mock.method(globalThis, "fetch", async () => {
      calls++;
      // Bound a broken implementation that retries forever.
      if (calls > 12)
        throw new ConnectError("retry guard", Code.PermissionDenied);
      return response(pb.WatchRunResponseSchema, [
        { case: "section", value: { attempt: { publicId: "attempt" } } },
        { case: "keepalive", value: {} },
        {
          case: "log",
          value: {
            offset: 0n,
            data: new TextEncoder().encode(progress && calls >= 4 ? "ab" : "a"),
          },
        },
        {
          case: "log",
          value: { offset: BigInt(log.offset), data: new Uint8Array() },
        },
      ]);
    });
    const schedule = globalThis.setTimeout;
    t.mock.method(globalThis, "setTimeout", (callback, ms, ...args) =>
      schedule(callback, ms >= 500 ? 0 : ms, ...args),
    );
    await assert.rejects(
      watchRun({ id: "run", repo: "alice/demo", log, follow: true }),
      (err) => err.code === Code.Unavailable,
    );
    assert.equal(calls, progress ? 9 : 6);
    assert.equal(log.offset, progress ? 2 : 1);
    assert.equal(log.attempts.length, 1);
  });
}
