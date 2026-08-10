export const WORKER_PROTOCOL_VERSION = 1 as const;
export const WORKER_VERSION = "0.0.0";

type Format = "markdown" | "raw_html";
export type HelloMessage = Readonly<{ type: "hello"; protocol_version: typeof WORKER_PROTOCOL_VERSION; worker_version: string; capabilities: readonly ["extract"] }>;
export type ExtractOptions = Readonly<{ formats: readonly Format[]; only_main_content: boolean; wait_for_ms: number; max_output_chars: number }>;
export type RequestMessage = Readonly<{ type: "request"; protocol_version: typeof WORKER_PROTOCOL_VERSION; request_id: string; operation: "extract" | "render"; url?: string; deadline_ms: number; spool_id: string; options: ExtractOptions }>;
export type CancelMessage = Readonly<{ type: "cancel"; protocol_version: typeof WORKER_PROTOCOL_VERSION; request_id: string }>;
export type Artifact = Readonly<{ kind: Format; path: string; media_type: string; bytes: number; sha256: string }>;
export type ResultMessage = Readonly<{ type: "result"; protocol_version: typeof WORKER_PROTOCOL_VERSION; request_id: string; status: "ok"; final_url?: string; artifacts: readonly Artifact[]; warnings: readonly string[]; timing_ms: Readonly<Record<string, number>> }>;
export type ErrorMessage = Readonly<{ type: "error"; protocol_version: typeof WORKER_PROTOCOL_VERSION; request_id: string; status: "error"; code: string; message: string; retryable: boolean }>;
export type InputMessage = RequestMessage | CancelMessage;

export function helloMessage(): HelloMessage { return { type: "hello", protocol_version: 1, worker_version: WORKER_VERSION, capabilities: ["extract"] }; }
export function errorMessage(requestId: string, code: string, message: string, retryable = false): ErrorMessage { return { type: "error", protocol_version: 1, request_id: requestId || "unknown", status: "error", code, message, retryable }; }
function record(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function requestId(value: unknown): value is string { return typeof value === "string" && value.length >= 1 && value.length <= 128; }
const formats: Format[] = ["markdown", "raw_html"];
function options(value: unknown): value is ExtractOptions {
  if (!record(value) || !Array.isArray(value.formats) || value.formats.length < 1 || value.formats.length > 7 || new Set(value.formats).size !== value.formats.length || typeof value.only_main_content !== "boolean" || !Number.isInteger(value.wait_for_ms) || value.wait_for_ms < 0 || value.wait_for_ms > 30000 || !Number.isInteger(value.max_output_chars) || value.max_output_chars < 1 || value.max_output_chars > 100000) return false;
  return value.formats.every((format) => formats.includes(format as Format));
}
export function parseInputFrame(line: string): InputMessage {
  let value: unknown; try { value = JSON.parse(line); } catch { throw new Error("malformed_json"); }
  if (!record(value) || value.protocol_version !== 1 || !["request", "cancel"].includes(value.type as string)) throw new Error("invalid_frame");
  if (!requestId(value.request_id)) throw new Error("invalid_request_id");
  if (value.type === "cancel") return value as CancelMessage;
  if (!["extract", "render"].includes(value.operation as string) || !Number.isInteger(value.deadline_ms) || value.deadline_ms < 1 || value.deadline_ms > 300000 || typeof value.spool_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(value.spool_id) || !options(value.options)) throw new Error("invalid_request");
  return value as RequestMessage;
}
