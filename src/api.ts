// One door to the Rust core. In the desktop app it is Tauri IPC; in a plain
// browser (development only) it is the local dev bridge on 127.0.0.1:1421.
import { invoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

const isTauri = "__TAURI_INTERNALS__" in window;
const BRIDGE = "http://127.0.0.1:1421";

export class AppError extends Error {
  code: string;
  detail: any;
  constructor(e: any) {
    super(e?.message ?? String(e));
    this.code = e?.code ?? "error";
    this.detail = e?.detail;
  }
}

export async function rpc<T = any>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) {
    try {
      return await invoke<T>("rpc", { cmd, args });
    } catch (e) {
      throw new AppError(e);
    }
  }
  let res: Response;
  try {
    res = await fetch(`${BRIDGE}/rpc`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ cmd, args }) });
  } catch {
    throw new AppError({ code: "bridge", message: "جسر التطوير غير متصل (pnpm devserver)" });
  }
  const body = await res.json();
  if (!body.ok) throw new AppError(body.error);
  return body.result as T;
}

export function assetUrl(id: string): string {
  return isTauri ? `seedasset://localhost/${id}` : `${BRIDGE}/asset/${id}`;
}

type Handler = (payload: any) => void;
const subscribers = new Map<string, Set<Handler>>();
let polling = false;

async function poll() {
  let since = Number.MAX_SAFE_INTEGER;
  for (;;) {
    try {
      const r = await (await fetch(`${BRIDGE}/events?since=${since}`)).json();
      since = r.next;
      for (const e of r.events) subscribers.get(e.event)?.forEach((h) => h(e.payload));
    } catch {
      /* bridge down; keep trying */
    }
    await new Promise((ok) => setTimeout(ok, 150));
  }
}

export function listen(event: string, handler: Handler): () => void {
  if (isTauri) {
    const un = tauriListen(event, (e) => handler(e.payload));
    return () => void un.then((f) => f());
  }
  if (!subscribers.has(event)) subscribers.set(event, new Set());
  subscribers.get(event)!.add(handler);
  if (!polling) {
    polling = true;
    void poll();
  }
  return () => void subscribers.get(event)?.delete(handler);
}

export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",")[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });
}

export function pickFile(accept: string): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = () => resolve(input.files?.[0] ?? null);
    input.oncancel = () => resolve(null);
    input.click();
  });
}

export function pickFiles(accept: string): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.multiple = true;
    input.onchange = () => resolve(Array.from(input.files ?? []));
    input.oncancel = () => resolve([]);
    input.click();
  });
}

export function pickImage(): Promise<File | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/png,image/jpeg,image/webp";
    input.onchange = () => resolve(input.files?.[0] ?? null);
    input.oncancel = () => resolve(null);
    input.click();
  });
}
