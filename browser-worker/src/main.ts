import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { join, resolve, relative, isAbsolute } from "node:path";
import { createInterface } from "node:readline";
import { errorMessage, helloMessage, parseInputFrame } from "./protocol.ts";

const root = resolve(process.env.MCP_SEARCH_SPOOL_ROOT || process.cwd());
const active = new Map();
const output = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

function artifact(request, kind, content) {
  const bounded = content.slice(0, request.options.max_output_chars);
  const filename = `${request.request_id}-${kind}.${kind === "markdown" ? "md" : "html"}`;
  const dir = resolve(root, request.spool_id);
  const path = join(dir, filename);
  writeFileSync(path, bounded, { encoding: "utf8", flag: "wx" });
  const bytes = Buffer.byteLength(bounded);
  return { kind, path: relative(dir, path), media_type: kind === "markdown" ? "text/markdown" : "text/html", bytes, sha256: createHash("sha256").update(bounded).digest("hex") };
}

function extract(request, signal) {
  if (request.operation === "render") throw new Error("render_unavailable");
  const dir = resolve(root, request.spool_id);
  if (!existsSync(dir) || !statSync(dir).isDirectory()) throw new Error("spool_not_found");
  const input = resolve(dir, "input.html");
  if (isAbsolute(relative(dir, input)) || !existsSync(input) || realpathSync(input) !== input) throw new Error("input_artifact_not_found");
  if (signal.aborted) throw new Error("cancelled");
  const html = readFileSync(input, "utf8");
  const artifacts = [];
  for (const format of request.options.formats) {
    if (format === "raw_html") artifacts.push(artifact(request, format, html));
    else if (format === "markdown") {
      const withoutEmbeddedContent = html.replace(/<script[\s\S]*?<\/script>|<style[\s\S]*?<\/style>/gi, "");
      const text = withoutEmbeddedContent.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
      artifacts.push(artifact(request, format, text));
    } else {
      throw new Error(`unsupported_format:${format}`);
    }
  }
  return { type: "result", protocol_version: 1, request_id: request.request_id, status: "ok", artifacts, warnings: [], timing_ms: { extraction: 0 } };
}

async function handle(line) {
  let input;
  try { input = parseInputFrame(line); } catch (error) { output(errorMessage("unknown", error instanceof Error ? error.message : "invalid_frame", "Invalid worker protocol frame")); return; }
  if (input.type === "cancel") {
    const request = active.get(input.request_id);
    if (request) request.abort();
    output(errorMessage(input.request_id, "cancelled", "Request cancelled"));
    return;
  }
  const controller = new AbortController(); active.set(input.request_id, controller);
  try { output(extract(input, controller.signal)); } catch (error) { const code = error instanceof Error ? error.message : "worker_failure"; const retryable = code === "render_unavailable"; output(errorMessage(input.request_id, code, code === "render_unavailable" ? "Render is unavailable because Playwright is not installed" : code, retryable)); } finally { active.delete(input.request_id); }
}

output(helloMessage());
createInterface({ input: process.stdin, crlfDelay: Infinity }).on("line", (line) => { void handle(line); });
