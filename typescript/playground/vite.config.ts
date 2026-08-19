import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

/**
 * Where the demo document list can live, in preference order:
 *
 * - the in-process demo (`cleverhans-demo serve`) serves it beside the WS
 *   mount on 8787;
 * - in the standalone topology, 8787 is `cleverhans serve` — which has no
 *   document store — and the list lives on the webhook host (8791).
 *
 * `DOCS_ORIGIN` overrides both for anything else.
 */
const DOCS_ORIGINS: string[] = [
  process.env["DOCS_ORIGIN"],
  "http://127.0.0.1:8787",
  "http://127.0.0.1:8791",
].filter((origin): origin is string => typeof origin === "string" && origin.length > 0);

/**
 * Dev-only same-origin `GET /documents`, proxied to the first backend that
 * answers.
 *
 * Two footguns die here. The browser never makes a cross-origin request, so
 * pointing at the wrong port can no longer surface as an opaque
 * "access control checks" failure; and picking the port is the dev server's
 * job, not an env var the topology has to remember.
 */
function documentsProxy(): Plugin {
  let lastGood: string | null = null;
  return {
    name: "cleverhans-documents-proxy",
    configureServer(server) {
      server.middlewares.use("/documents", (_req, res) => {
        const ordered = lastGood
          ? [lastGood, ...DOCS_ORIGINS.filter((origin) => origin !== lastGood)]
          : DOCS_ORIGINS;
        void (async () => {
          for (const origin of ordered) {
            let upstream: Response;
            try {
              upstream = await fetch(`${origin}/documents`);
            } catch {
              continue; // nothing listening there
            }
            if (!upstream.ok) {
              continue; // listening, but not the document store
            }
            if (origin !== lastGood) {
              server.config.logger.info(`  ➜  documents: ${origin}/documents`);
              lastGood = origin;
            }
            res.setHeader("content-type", "application/json");
            res.end(await upstream.text());
            return;
          }
          res.statusCode = 503;
          res.setHeader("content-type", "application/json");
          res.end(
            JSON.stringify({
              error: `no demo backend answered GET /documents (tried ${DOCS_ORIGINS.join(", ")})`,
            }),
          );
        })();
      });
    },
  };
}

export default defineConfig({
  plugins: [react(), documentsProxy()],
});
