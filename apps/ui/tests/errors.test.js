import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

test("errors: a server answer carries code and request id, a script error is the browser's", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { describeError } = await vite.ssrLoadModule("/src/lib/errors.ts");
  const enc = (s) => Buffer.from(s);
  const str = (field, s) =>
    Buffer.concat([Buffer.from([(field << 3) | 2, s.length]), enc(s)]);
  const entry = Buffer.concat([str(1, "request_id"), str(2, "01ABC")]);
  const info = Buffer.concat([
    str(2, "kajiba"),
    Buffer.from([(3 << 3) | 2, entry.length]),
    entry,
  ]);
  const server = describeError(
    {
      code: 13,
      rawMessage: "boom",
      message: "[internal] boom",
      details: [{ type: "google.rpc.ErrorInfo", value: new Uint8Array(info) }],
    },
    "/a/b",
  );
  assert.equal(server.kind, "server");
  assert.equal(server.requestId, "01ABC");
  assert.match(server.details, /code: internal[\s\S]*request_id: 01ABC/);
  const browser = describeError(new TypeError("x is undefined"), "/a/b");
  assert.equal(browser.kind, "browser");
  assert.match(browser.details, /message: x is undefined\nroute: \/a\/b\n/);
  assert.doesNotMatch(browser.details, /server/i);
});
