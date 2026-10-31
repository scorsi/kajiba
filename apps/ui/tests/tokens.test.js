import assert from "node:assert/strict";
import { test } from "node:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createServer } from "vite";

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
    const schema = pb[`${method}ResponseSchema`];
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
