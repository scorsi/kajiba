import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { expect, test, type Page } from "@playwright/test";
import {
  GetRunResponseSchema,
  RunState,
  WatchRunResponseSchema,
} from "../src/lib/gen/kajiba/v1/run_pb";
import { authenticator, enroll, enrollLink } from "./support";

function frame(
  schema: typeof WatchRunResponseSchema | typeof GetRunResponseSchema,
  value: object,
) {
  const bytes = toBinary(schema, create(schema, value));
  const out = new Uint8Array(5 + bytes.length);
  new DataView(out.buffer).setUint32(1, bytes.length);
  out.set(bytes, 5);
  return [...out];
}
const section = (number: number, offset: number, running = false) =>
  frame(WatchRunResponseSchema, {
    event: {
      case: "section",
      value: {
        attempt: {
          publicId: `attempt-${number}`,
          number,
          state: running ? 1 : 2,
        },
        offset: BigInt(offset),
      },
    },
  });
const bytes = (offset: number, text: string) =>
  frame(WatchRunResponseSchema, {
    event: {
      case: "log",
      value: { offset: BigInt(offset), data: new TextEncoder().encode(text) },
    },
  });
const trailer = [
  128,
  0,
  0,
  0,
  16,
  ...new TextEncoder().encode("grpc-status: 0\r\n"),
];

async function fixture(page: Page, live = false) {
  await authenticator(page);
  await enroll(page, enrollLink());
  await page.getByRole("link", { name: "Continue to kajiba" }).click();
  const id = process.env.KAJIBA_E2E_PENDING_RUN;
  if (!id)
    throw new Error(
      "KAJIBA_E2E_PENDING_RUN is unset: run through nix run .#e2e",
    );
  const first = "older attempt\n";
  const second = Array.from(
    { length: 180 },
    (_, i) => `newer line ${i + 1}\n`,
  ).join("");
  const offset = new TextEncoder().encode(first).length;
  const end = offset + new TextEncoder().encode(second).length;
  const initial = [
    ...section(1, 0),
    ...bytes(0, first),
    ...section(2, offset, live),
    ...bytes(offset, second),
    ...trailer,
  ];
  const selected = [
    ...section(2, offset, live),
    ...bytes(offset, second),
    ...trailer,
  ];
  await page.route("**/kajiba.v1.RunService/GetRun", async (route) => {
    const response = await route.fetch();
    const body = await response.body();
    const length = body.readUInt32BE(1);
    const data = fromBinary(GetRunResponseSchema, body.subarray(5, 5 + length));
    data.run!.state = live ? RunState.RUNNING : RunState.SUCCEEDED;
    await route.fulfill({
      response,
      body: Buffer.from([...frame(GetRunResponseSchema, data), ...trailer]),
    });
  });
  await page.addInitScript(
    ({ initial, selected }) => {
      const original = window.fetch;
      const w = window as unknown as {
        emit: (frame: number[]) => void;
        watches: number;
        cancelled: number;
      };
      w.watches = 0;
      w.cancelled = 0;
      window.fetch = async (input, init) => {
        if (
          !String(input instanceof Request ? input.url : input).endsWith(
            "RunService/WatchRun",
          )
        )
          return original(input, init);
        w.watches++;
        const body = new Uint8Array(init!.body as ArrayBuffer);
        // follow is the final protobuf field (tag 3, boolean true).
        const following = body.at(-2) === 24 && body.at(-1) === 1;
        const resumed = body.includes(16); // offset field, absent at zero
        const headers = { "content-type": "application/grpc-web+proto" };
        if (following)
          return new Response(
            new ReadableStream({
              start(controller) {
                w.emit = (frame) => controller.enqueue(new Uint8Array(frame));
                init?.signal?.addEventListener(
                  "abort",
                  () => controller.close(),
                  { once: true },
                );
              },
            }),
            { headers },
          );
        if (w.watches > 1)
          await new Promise<void>((resolve, reject) => {
            const timer = setTimeout(resolve, 500);
            init?.signal?.addEventListener(
              "abort",
              () => {
                w.cancelled++;
                clearTimeout(timer);
                reject(new DOMException("Aborted", "AbortError"));
              },
              { once: true },
            );
          });
        return new Response(new Uint8Array(resumed ? selected : initial), {
          headers,
        });
      };
    },
    { initial, selected },
  );
  return { id, end };
}

test("runs open the latest attempt, anchors win, and switches load only logs", async ({
  page,
}) => {
  const { id } = await fixture(page);
  let metadata = 0;
  page.on("request", (req) => {
    if (req.url().endsWith("RunService/GetRun")) metadata++;
  });
  await page.goto(`/-/runs/${id}`);
  await expect(
    page.getByRole("region", { name: "attempt 2 log", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("log")).toContainText("newer line 180");
  const original = metadata;
  await page.locator('a[href="?step=1"]').first().click();
  await expect(
    page.getByRole("status", { name: "Loading attempt log" }),
  ).toBeVisible();
  await expect(page.locator('a[aria-busy="true"]')).toContainText("attempt 1");
  await page.locator('a[href="?step=2"]').first().click();
  await expect(page.getByRole("log")).toContainText("newer line 180");
  expect(metadata).toBe(original);
  expect(
    await page.evaluate(
      () => (window as unknown as { cancelled: number }).cancelled,
    ),
  ).toBeGreaterThan(0);
  await page.goto("about:blank");
  await page.goto(`/-/runs/${id}?step=2#L1-1`);
  await expect(
    page.getByRole("region", { name: "attempt 1 log", exact: true }),
  ).toBeVisible();
  await expect(page.locator("#L1-1")).toContainText("older attempt");
});

test("new log lines preserve a scrolled-up reader and follow only at the bottom", async ({
  page,
}) => {
  const { id, end } = await fixture(page, true);
  await page.goto(`/-/runs/${id}?pulse=0`);
  const pane = page.locator("[data-log-pane]");
  await expect(page.getByRole("log")).toContainText("newer line 180");
  await expect(
    page.getByRole("region", { name: "attempt 2 log", exact: true }),
  ).toContainText("running");
  await expect
    .poll(() =>
      page.evaluate(
        () => typeof (window as unknown as { emit?: unknown }).emit,
      ),
    )
    .toBe("function");
  await expect
    .poll(() =>
      pane.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight),
    )
    .toBeLessThan(5);
  await pane.evaluate((el) => {
    el.scrollTop = 200;
    el.dispatchEvent(new Event("scroll"));
  });
  const top = await pane.evaluate((el) => el.scrollTop);
  const emit = async (offset: number, text: string) =>
    page.evaluate(
      (frame) =>
        (window as unknown as { emit: (frame: number[]) => void }).emit(frame),
      bytes(offset, text),
    );
  // A resumed section precedes the live bytes, as on the real API.
  await page.evaluate(
    (frame) =>
      (window as unknown as { emit: (frame: number[]) => void }).emit(frame),
    section(2, 14, true),
  );
  await emit(end, "arrived while reading\n");
  await expect(page.getByRole("log")).toContainText("arrived while reading");
  expect(await pane.evaluate((el) => el.scrollTop)).toBe(top);
  await page.getByRole("button", { name: "Jump to latest" }).click();
  await expect
    .poll(() =>
      pane.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight),
    )
    .toBeLessThan(5);
  await emit(
    end + new TextEncoder().encode("arrived while reading\n").length,
    "follow this line\n",
  );
  await expect(page.getByRole("log")).toContainText("follow this line");
  await expect
    .poll(() =>
      pane.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight),
    )
    .toBeLessThan(5);
});
