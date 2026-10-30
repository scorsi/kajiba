import assert from "node:assert/strict";
import { test } from "node:test";
import { takeEnrollmentToken } from "../src/lib/enrollment.ts";

test("enrollment captures the token and replaces the URL before it is used", (t) => {
  const state = { navigation: "preserved" };
  const replace = t.mock.fn();
  globalThis.history = { state, replaceState: replace };
  t.after(() => {
    delete globalThis.history;
    delete globalThis.location;
  });

  for (const token of ["ab".repeat(32), ""]) {
    globalThis.location = new URL(
      `https://example.org/-/enroll?next=home#${token}`,
    );
    replace.mock.resetCalls();

    assert.equal(takeEnrollmentToken(), token);
    assert.equal(replace.mock.callCount(), 1);
    assert.deepEqual(replace.mock.calls[0].arguments, [
      state,
      "",
      "/-/enroll?next=home",
    ]);
  }
});
