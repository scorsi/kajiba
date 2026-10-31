/**
 * Tells a server answer from a failure inside the browser.
 *
 * A Connect error carries the gRPC code, the server's message and, in its
 * `google.rpc.ErrorInfo` detail, the `request_id` that the server log also
 * holds. Anything else (a render or script error) is the browser's own.
 */
export interface ErrorReport {
  /** `server`: the server answered with an error; `browser`: the page itself failed. */
  kind: "server" | "browser";
  code?: number;
  message: string;
  requestId?: string;
  /** Text to copy into a bug report. */
  details: string;
}

const CODE_NAMES = [
  "ok",
  "canceled",
  "unknown",
  "invalid_argument",
  "deadline_exceeded",
  "not_found",
  "already_exists",
  "permission_denied",
  "resource_exhausted",
  "failed_precondition",
  "aborted",
  "out_of_range",
  "unimplemented",
  "internal",
  "unavailable",
  "data_loss",
  "unauthenticated",
];
export const codeName = (code: number) => CODE_NAMES[code] ?? String(code);

/** Length-delimited fields (number → text) of a protobuf message; other wire types are skipped. */
function* fields(buf: Uint8Array): Generator<[number, Uint8Array]> {
  let i = 0;
  const varint = () => {
    let n = 0;
    for (let shift = 0; i < buf.length; shift += 7) {
      const b = buf[i++];
      n += (b & 0x7f) * 2 ** shift;
      if (!(b & 0x80)) break;
    }
    return n;
  };
  while (i < buf.length) {
    const tag = varint();
    const wire = tag & 7;
    if (wire === 2) {
      const len = varint();
      yield [tag >> 3, buf.subarray(i, i + len)];
      i += len;
    } else if (wire === 0) varint();
    else if (wire === 1) i += 8;
    else if (wire === 5) i += 4;
    else return;
  }
}

/** `request_id` from the metadata map of a serialized `google.rpc.ErrorInfo`. */
export function requestIdOf(value: Uint8Array): string | undefined {
  const text = new TextDecoder();
  for (const [field, map] of fields(value)) {
    if (field !== 3) continue;
    const kv = new Map([...fields(map)].map(([f, v]) => [f, text.decode(v)]));
    if (kv.get(1) === "request_id") return kv.get(2);
  }
  return undefined;
}

export function describeError(error: unknown, route = ""): ErrorReport {
  const e = error as {
    code?: unknown;
    rawMessage?: string;
    message?: string;
    stack?: string;
    details?: { type?: string; value?: Uint8Array }[];
  } | null;
  const message =
    e && typeof e === "object" ? (e.message ?? String(error)) : String(error);
  if (e && typeof e === "object" && typeof e.code === "number") {
    const info = e.details?.find((d) => d.type === "google.rpc.ErrorInfo");
    const requestId = info?.value ? requestIdOf(info.value) : undefined;
    const msg = e.rawMessage ?? message;
    return {
      kind: "server",
      code: e.code,
      message: msg,
      requestId,
      details: [
        `code: ${codeName(e.code)}`,
        `message: ${msg}`,
        `request_id: ${requestId ?? "-"}`,
        `route: ${route}`,
      ].join("\n"),
    };
  }
  const stack = (e?.stack ?? "").split("\n").slice(0, 6).join("\n");
  return {
    kind: "browser",
    message,
    details: [`message: ${message}`, `route: ${route}`, stack].join("\n"),
  };
}
