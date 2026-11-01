import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "vite";

const enc = new TextEncoder();

test("run log decoding, line splitting and resume offsets", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { RunLog } = await vite.ssrLoadModule("/src/lib/runlog.ts");

  const bytes = enc.encode("héllo\n\x1b[31merror: boom\x1b[0m\nlast");
  const log = new RunLog();
  log.section("a1", "one");
  // chunks split a line and the two bytes of "é"
  log.bytes(0, bytes.subarray(0, 2));
  log.bytes(2, bytes.subarray(2, 9));
  log.bytes(9, bytes.subarray(9));
  assert.equal(log.offset, bytes.length);
  assert.deepEqual(
    log.lines(0).map((l) => [l.text, l.stream]),
    [
      ["héllo", "out"],
      ["error: boom", "err"],
      ["last", "out"],
    ],
  );

  // a resumed stream repeats the section and may overlap received bytes
  log.section("a1", "one");
  log.bytes(bytes.length - 2, enc.encode("st\n and more\n"));
  assert.equal(log.attempts.length, 1);
  assert.deepEqual(
    log.lines(0).map((l) => l.text),
    ["héllo", "error: boom", "last", " and more"],
  );

  // a new attempt is a new column; progress redraws keep their last state
  log.section("a2", "two");
  log.bytes(log.offset, enc.encode("10%\r50%\r100%\n"));
  assert.equal(log.attempts.length, 2);
  assert.deepEqual(
    log.lines(1).map((l) => l.text),
    ["100%"],
  );
});

test("line times follow production chunks, partial lines, legacy logs and retries", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const { RunLog } = await vite.ssrLoadModule("/src/lib/runlog.ts");
  const log = new RunLog();
  log.section("a1", "first", 10000);
  log.bytes(0, enc.encode("one\ntw"), 11000);
  assert.deepEqual(
    log.lines(0).map((l) => l.offsetSeconds),
    [1, 1],
  );
  log.bytes(6, enc.encode("o\nthree\npartial"), 16000);
  assert.deepEqual(
    log.lines(0).map((l) => l.offsetSeconds),
    [1, 6, 6, 6],
  );
  log.bytes(6, enc.encode("o\nthree\npartial"), 99000);
  assert.deepEqual(
    log.lines(0).map((l) => l.offsetSeconds),
    [1, 6, 6, 6],
  );
  log.bytes(log.offset, enc.encode("\nlegacy\n"));
  assert.deepEqual(
    log.lines(0).map((l) => l.offsetSeconds),
    [1, 6, 6, 0, 0],
  );
  log.section("a2", "replay", 20000);
  log.bytes(log.offset, enc.encode("new\n"), 21500);
  log.bytes(log.offset, enc.encode("clock skew\n"), 19000);
  assert.deepEqual(
    log.lines(1).map((l) => l.offsetSeconds),
    [1.5, 0],
  );
});
