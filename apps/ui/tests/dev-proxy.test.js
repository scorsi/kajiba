import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { createServer } from "vite";
import { DEFAULT_URL, devProxy } from "../dev-proxy.js";

const TOKEN = "kjb_" + "A".repeat(43);

/** A backend recording each request's method, path and headers. */
async function backend(t) {
  const seen = [];
  const server = http.createServer((req, res) => {
    seen.push({ method: req.method, url: req.url, headers: req.headers });
    res.end("backend");
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());
  return { url: `http://127.0.0.1:${server.address().port}`, seen };
}

/** Vite on a loopback port with the dev proxy of `env`. */
async function vite(t, env) {
  const server = await createServer({
    configFile: false,
    logLevel: "silent",
    server: { host: "127.0.0.1", port: 0, proxy: devProxy(env) },
  });
  await server.listen();
  t.after(() => server.close());
  return `http://127.0.0.1:${server.httpServer.address().port}`;
}

/** POST through Vite as the browser would, with its origin and cookie. */
function post(base, path) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      `${base}${path}`,
      {
        method: "POST",
        headers: {
          origin: "http://localhost:5173",
          cookie: "kajiba_session=x",
        },
      },
      (res) => {
        let body = "";
        res.on("data", (chunk) => (body += chunk));
        res.on("end", () => resolve(body));
      },
    );
    req.on("error", reject);
    req.end("{}");
  });
}

test("the default backend is the local server, without a token", () => {
  const proxy = devProxy({});
  assert.deepEqual(Object.keys(proxy), ["^/kajiba\\.v1\\.", "/auth/webauthn/"]);
  assert.equal(proxy["/auth/webauthn/"].target, DEFAULT_URL);
  assert.equal(DEFAULT_URL, "http://127.0.0.1:8080");
});

test("a token file for another URL or over remote http is refused", () => {
  const file = (content) => () => content;
  assert.throws(
    () =>
      devProxy(
        {
          KAJIBA_DEV_URL: "https://b.example.org",
          KAJIBA_DEV_TOKEN_FILE: "/t",
        },
        file(`https://a.example.org\n${TOKEN}\n`),
      ),
    /is for https:\/\/a\.example\.org, not KAJIBA_DEV_URL https:\/\/b\.example\.org/,
  );
  assert.throws(
    () =>
      devProxy(
        {
          KAJIBA_DEV_URL: "http://forge.example.org",
          KAJIBA_DEV_TOKEN_FILE: "/t",
        },
        file(`http://forge.example.org\n${TOKEN}\n`),
      ),
    /refusing to send a token to http:\/\/forge\.example\.org/,
  );
  devProxy(
    {
      KAJIBA_DEV_URL: "https://forge.example.org",
      KAJIBA_DEV_TOKEN_FILE: "/t",
    },
    file(`https://forge.example.org\n${TOKEN}\n`),
  );
});

test("pnpm dev proxies grpc-web and ceremonies to the local server as the browser sent them", async (t) => {
  const server = await backend(t);
  const base = await vite(t, { KAJIBA_DEV_URL: server.url });
  for (const path of [
    "/kajiba.v1.AuthService/GetViewer",
    "/auth/webauthn/login/start",
  ]) {
    assert.equal(await post(base, path), "backend");
  }
  assert.deepEqual(
    server.seen.map((r) => r.url),
    ["/kajiba.v1.AuthService/GetViewer", "/auth/webauthn/login/start"],
  );
  for (const { headers } of server.seen) {
    assert.equal(headers.origin, "http://localhost:5173");
    assert.equal(headers.cookie, "kajiba_session=x");
    assert.equal(headers.authorization, undefined);
  }
  // Anything else stays with Vite.
  await post(base, "/kajiba.v2.Other/Call");
  assert.equal(server.seen.length, 2);
});

test("with KAJIBA_DEV_TOKEN_FILE, the PAT replaces the browser's origin and cookie", async (t) => {
  const server = await backend(t);
  const dir = mkdtempSync(join(tmpdir(), "kajiba-dev-"));
  t.after(() => rmSync(dir, { recursive: true }));
  const tokenFile = join(dir, "token");
  writeFileSync(tokenFile, `${server.url}\n${TOKEN}\n`, { mode: 0o600 });
  const base = await vite(t, {
    KAJIBA_DEV_URL: server.url,
    KAJIBA_DEV_TOKEN_FILE: tokenFile,
  });
  assert.equal(await post(base, "/kajiba.v1.RepoService/ListRepos"), "backend");
  const [{ headers }] = server.seen;
  assert.equal(headers.authorization, `Bearer ${TOKEN}`);
  assert.equal(headers.origin, undefined);
  assert.equal(headers.cookie, undefined);
});
