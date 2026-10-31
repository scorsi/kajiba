import assert from "node:assert/strict";
import { test } from "node:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createServer } from "vite";

/** A grpc-web answer carrying `response` as `schema`, then an OK trailer. */
function grpcWeb(schema, response) {
  const bytes = toBinary(schema, create(schema, response));
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

test("account token actions use TokenService and preserve server metadata", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const api = await vite.ssrLoadModule("/src/lib/api.ts");
  const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/token_pb.ts");
  const { Scope } = await vite.ssrLoadModule(
    "/src/lib/gen/kajiba/v1/options_pb.ts",
  );
  const runtime = {
    publicId: "runtime-id",
    name: "laptop",
    scopes: [Scope.REPO_READ, Scope.ADMIN],
    source: pb.TokenSource.RUNTIME,
    createdAt: { seconds: 10n },
    expiresAt: { seconds: 100n },
    lastUsedAt: { seconds: 20n },
  };
  const declared = {
    ...runtime,
    publicId: "config-id",
    source: pb.TokenSource.CONFIG,
  };
  const revoked = { ...runtime, revokedAt: { seconds: 30n } };
  const calls = [];
  let fail = false;
  t.mock.method(globalThis, "fetch", async (input, init) => {
    const method = String(input).split("/").at(-1);
    calls.push(method);
    if (fail) return new Response("unavailable", { status: 503 });
    const body = new Uint8Array(init.body);
    const request = fromBinary(pb[`${method}RequestSchema`], body.subarray(5));
    let response;
    if (method === "ListTokens") {
      response =
        request.pageToken === "next"
          ? { tokens: [declared], nextPageToken: "" }
          : { tokens: [revoked], nextPageToken: "next" };
    } else if (method === "CreateToken") {
      assert.equal(request.name, "laptop");
      assert.deepEqual(request.scopes, runtime.scopes);
      assert.equal(request.expiresAt.seconds, 100n);
      response = { token: runtime, secret: "kjb_server_generated_secret" };
    } else {
      assert.equal(method, "RevokeToken");
      assert.equal(request.publicId, "runtime-id");
      response = { token: revoked };
    }
    return grpcWeb(pb[`${method}ResponseSchema`], response);
  });

  const { tokens } = await api.listTokens();
  assert.equal(tokens.length, 2);
  assert.equal(tokens[0].id, "runtime-id");
  assert.equal(tokens[0].revokedTime.seconds, 30n);
  assert.deepEqual(tokens[0].scopes, ["repo:read", "admin"]);
  assert.equal(tokens[0].lastUsedTime.seconds, 20n);
  assert.equal(tokens[1].declared, true);
  const created = await api.createToken({
    name: "laptop",
    scopes: ["repo:read", "admin"],
    expireTime: { seconds: 100n, nanos: 0 },
  });
  assert.equal(created.secret, "kjb_server_generated_secret");
  assert.equal(created.token.id, "runtime-id");
  await api.revokeToken({ id: created.token.id });
  assert.deepEqual(calls, [
    "ListTokens",
    "ListTokens",
    "CreateToken",
    "RevokeToken",
  ]);
  fail = true;
  await assert.rejects(api.listTokens());
  await assert.rejects(
    api.createToken({
      name: "laptop",
      scopes: ["repo:read"],
      expireTime: { seconds: 100n, nanos: 0 },
    }),
  );
  await assert.rejects(api.revokeToken({ id: "runtime-id" }));
});

test("account passkeys and declared keys use PasskeyService and GetViewer", async (t) => {
  const vite = await createServer({
    configFile: false,
    server: { middlewareMode: true, ws: false },
  });
  t.after(() => vite.close());
  const api = await vite.ssrLoadModule("/src/lib/api.ts");
  const pb = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/passkey_pb.ts");
  const auth = await vite.ssrLoadModule("/src/lib/gen/kajiba/v1/auth_pb.ts");
  const calls = [];
  t.mock.method(globalThis, "fetch", async (input, init) => {
    const method = String(input).split("/").at(-1);
    const schemas = method === "GetViewer" ? auth : pb;
    const request = fromBinary(
      schemas[`${method}RequestSchema`],
      new Uint8Array(init.body).subarray(5),
    );
    calls.push([method, request.pageToken ?? request.publicId]);
    const response =
      method === "GetViewer"
        ? {
            user: "alice",
            keys: [
              {
                usage: auth.SshKeyUsage.AUTH,
                keyType: "ssh-ed25519",
                fingerprint: "SHA256:a",
              },
              {
                usage: auth.SshKeyUsage.SIGNING,
                keyType: "ssh-ed25519",
                fingerprint: "SHA256:s",
              },
            ],
          }
        : method === "DeletePasskey"
          ? {}
          : request.pageToken === ""
            ? {
                passkeys: [
                  { publicId: "first", name: "", createdAt: { seconds: 10n } },
                ],
                nextPageToken: "next",
              }
            : {
                passkeys: [
                  {
                    publicId: "second",
                    name: "backup",
                    createdAt: { seconds: 20n },
                    lastUsedAt: { seconds: 30n },
                  },
                ],
              };
    return grpcWeb(schemas[`${method}ResponseSchema`], response);
  });

  const { passkeys } = await api.listPasskeys();
  assert.deepEqual(
    passkeys.map((p) => [p.id, p.name, p.lastUsedTime?.seconds]),
    [
      ["first", "", undefined],
      ["second", "backup", 30n],
    ],
  );
  await api.deletePasskey({ id: "first" });
  const viewer = await api.getViewer();
  assert.deepEqual(viewer.keys, [
    { usage: "auth", keyType: "ssh-ed25519", fingerprint: "SHA256:a" },
    { usage: "signing", keyType: "ssh-ed25519", fingerprint: "SHA256:s" },
  ]);
  assert.deepEqual(calls, [
    ["ListPasskeys", ""],
    ["ListPasskeys", "next"],
    ["DeletePasskey", "first"],
    ["GetViewer", undefined],
  ]);
});
