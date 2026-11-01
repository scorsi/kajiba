import assert from "node:assert/strict";
import { test } from "node:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createServer } from "vite";

const frame = (schema, event) => {
  const bytes = toBinary(schema, create(schema, { event }));
  const out = new Uint8Array(5 + bytes.length);
  new DataView(out.buffer).setUint32(1, bytes.length);
  out.set(bytes, 5);
  return out;
};

test("latest attempt is default, anchors win and bottom means within four pixels", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { focusStep, atLogBottom } = await vite.ssrLoadModule(
    "/src/lib/data/run.ts",
  );
  const run = { steps: [{ state: 4 }, { state: 3 }, { state: 3 }] };
  assert.equal(focusStep(run), 2);
  assert.equal(focusStep(run, "1"), 0);
  assert.equal(focusStep(run, "1", "#L2-7"), 1);
  assert.equal(focusStep(run, "99"), 2);
  assert.equal(focusStep(run, "1.5"), 2);
  assert.equal(focusStep(run, null, "#L0-7"), 2);
  assert.equal(atLogBottom(1000, 396, 600), true);
  assert.equal(atLogBottom(1000, 395, 600), false);
});

test("attempt reads start at their section, stop before another and accept cancellation", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { readAttemptLog } = await vite.ssrLoadModule("/src/lib/api.ts");
  const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/run_pb.ts");
  let transportSignal;
  t.mock.method(globalThis, "fetch", async (_, init) => {
    transportSignal = init.signal;
    const body = new Uint8Array(init.body);
    const req = fromBinary(pb.WatchRunRequestSchema, body.subarray(5));
    assert.equal(req.offset, 40n);
    assert.equal(req.follow, false);
    const events = [
      {
        case: "section",
        value: { attempt: { publicId: "two", number: 2 }, offset: 40n },
      },
      {
        case: "log",
        value: { offset: 40n, data: new TextEncoder().encode("second\n") },
      },
      {
        case: "section",
        value: { attempt: { publicId: "three", number: 3 }, offset: 47n },
      },
    ];
    return new Response(
      new ReadableStream({
        start(controller) {
          for (const event of events)
            controller.enqueue(frame(pb.WatchRunResponseSchema, event));
        },
      }),
      { headers: { "content-type": "application/grpc-web+proto" } },
    );
  });
  const req = {
    id: "run",
    repo: "alice/demo",
    attempt: "two",
    offset: 40,
    signal: new AbortController().signal,
  };
  assert.deepEqual(
    (await readAttemptLog(req)).map((l) => l.text),
    ["second"],
  );
  assert.equal(transportSignal.aborted, true);
  let signal;
  t.mock.method(globalThis, "fetch", async (_, init) => {
    signal = init.signal;
    return new Promise((_, reject) =>
      init.signal.addEventListener("abort", () => reject(init.signal.reason), {
        once: true,
      }),
    );
  });
  const stop = new AbortController();
  const pending = readAttemptLog({ ...req, signal: stop.signal });
  await new Promise((done) => setTimeout(done, 0));
  stop.abort();
  await pending;
  assert.equal(signal.aborted, true);
});
