// `pnpm dev`'s backend (ADR 0102): grpc-web and the ceremony routes go to KAJIBA_DEV_URL,
// with the PAT of KAJIBA_DEV_TOKEN_FILE for a remote instance. Never part of the bundle.
// @ts-nocheck: Node APIs without @types/node, which is not a dependency (ADR 0092).
import { readFileSync } from "node:fs";

export const DEFAULT_URL = "http://127.0.0.1:8080";

/** `https`, or plain `http` to this machine: `pnpm dev` is development mode (ADRs 0099, 0104). */
function secure(url) {
  return (
    url.protocol === "https:" ||
    (url.protocol === "http:" &&
      ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
  );
}

/**
 * Vite's `server.proxy` for `env`; throws on a token it must not send, warns once when it
 * sends one over plain http.
 * @returns {Record<string, import("vite").ProxyOptions>}
 */
export function devProxy(
  env,
  read = (path) => readFileSync(path, "utf8"),
  warn = console.warn,
) {
  const target = env.KAJIBA_DEV_URL || DEFAULT_URL;
  const tokenFile = env.KAJIBA_DEV_TOKEN_FILE;
  let token;
  if (tokenFile) {
    // A `kajiba login` credential file: forge URL, then token (ADR 0098).
    const [url = "", saved = ""] = read(tokenFile).split("\n");
    if (url.trim() !== target) {
      throw new Error(
        `KAJIBA_DEV_TOKEN_FILE ${tokenFile} is for ${url.trim()}, not KAJIBA_DEV_URL ${target}`,
      );
    }
    const parsed = new URL(target);
    if (!secure(parsed)) {
      throw new Error(
        `refusing to send a credential to ${target}: the URL must be https (http only to localhost, 127.0.0.1 or [::1], in development mode)`,
      );
    }
    if (parsed.protocol === "http:") {
      warn(
        `warning: development mode: sending the token over plain http to ${target}`,
      );
    }
    token = saved.trim();
  }
  const options = {
    target,
    changeOrigin: true,
    configure: (proxy) => {
      if (token === undefined) return;
      proxy.on("proxyReq", (request) => {
        // The PAT authenticates; the Vite origin and its cookies mean nothing remotely.
        request.setHeader("authorization", `Bearer ${token}`);
        request.removeHeader("origin");
        request.removeHeader("cookie");
      });
    },
  };
  return { "^/kajiba\\.v1\\.": options, "/auth/webauthn/": options };
}
