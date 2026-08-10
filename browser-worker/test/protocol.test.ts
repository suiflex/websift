import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { helloMessage, parseInputFrame, WORKER_PROTOCOL_VERSION } from "../src/protocol.ts";

const request = (overrides: Record<string, unknown> = {}) => ({ type: "request", protocol_version: 1, request_id: "req-1", operation: "extract", deadline_ms: 1000, spool_id: "spool_1", options: { formats: ["markdown"], only_main_content: true, wait_for_ms: 0, max_output_chars: 1000 }, ...overrides });

test("hello advertises only implemented capabilities", () => assert.deepEqual(helloMessage(), { type: "hello", protocol_version: WORKER_PROTOCOL_VERSION, worker_version: "0.0.0", capabilities: ["extract"] }));
test("parses extract and cancellation frames", () => { assert.equal(parseInputFrame(JSON.stringify(request())).operation, "extract"); assert.equal(parseInputFrame(JSON.stringify({ type: "cancel", protocol_version: 1, request_id: "req-1" })).type, "cancel"); });
test("rejects malformed frames and unsupported formats", () => { assert.throws(() => parseInputFrame("{"), /malformed_json/); assert.throws(() => parseInputFrame(JSON.stringify({ type: "request", protocol_version: 2 })), /invalid_frame/); assert.throws(() => parseInputFrame(JSON.stringify(request({ spool_id: "../escape" }))), /invalid_request/); assert.throws(() => parseInputFrame(JSON.stringify(request({ options: { ...request().options, formats: ["links"] } }))), /invalid_request/); });

test("markdown extraction removes embedded content and normalizes whitespace", () => {
  const spoolRoot = mkdtempSync(join(tmpdir(), "websift-worker-"));
  const spool = join(spoolRoot, "spool_1");
  mkdirSync(spool);
  writeFileSync(join(spool, "input.html"), "<html>\n<script>bad()</script><style>.bad {}</style><main>  Hello\n\t world </main>\n</html>");
  const result = spawnSync(process.execPath, ["--experimental-strip-types", "src/main.ts"], {
    cwd: new URL("..", import.meta.url).pathname,
    input: `${JSON.stringify(request())}\n`,
    encoding: "utf8",
    env: { ...process.env, WEBSIFT_SPOOL_ROOT: realpathSync(spoolRoot) },
  });
  assert.equal(result.status, 0, result.stderr);
  const frames = result.stdout.trim().split("\n").map((line) => JSON.parse(line));
  assert.equal(frames[1].status, "ok");
  assert.equal(frames[1].artifacts[0].bytes, Buffer.byteLength("Hello world"));
});
