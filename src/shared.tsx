import { createContext, useContext } from "react";
import { assetUrl } from "./api";

export type AppState = {
  theme: "light" | "dark" | "system";
  providerMode: "modelark" | "mock";
  region: string;
  regions: Record<string, string>;
  /** Rules of the selected connection when it is a budgeted gateway, else null. */
  gateway: any;
  /** The owner's own prices (USD), used only for estimates: per million video tokens, per image. */
  prices: { videoPerM: number | null; imageEach: number | null };
  /** The owner declared their reference images AI-generated: new ones are added to the gateway library. */
  libraryUpload: boolean;
  gateways: Record<string, any>;
  hasKey: Record<string, boolean>;
  chatModel: string;
  imageModel: string;
  thinking: string;
  models: ModelInfo[];
  lastProjectId: string | null;
};
export type ModelInfo = {
  key: string; label: string; kind: "chat" | "image" | "video"; candidateIds: string[]; resolvedId: string;
  endpoint: string; status: string; options: any;
};
export type Project = { id: string; name: string; instructions: string; assetCount: number; threadCount: number };
export type Shell = {
  app: AppState;
  setApp: (a: AppState) => void;
  toast: (text: string, kind?: "info" | "error") => void;
  /** Show the run plan; resolves when the dialog closes (true if something ran). */
  reviewPlan: (plan: any, threadId?: string) => Promise<boolean>;
  viewAsset: (id: string | null) => void;
  resolvedTheme: "light" | "dark";
  goCanvas: (nodeId?: string) => void;
  openSettings: () => void;
};
export const ShellCtx = createContext<Shell>(null as any);
export const useShell = () => useContext(ShellCtx);

export const STATUS_LABEL: Record<string, string> = {
  unchecked: "Not checked", visible: "Listed", tried: "Working", needs_activation: "No access", not_visible: "Not listed",
};

export function ProviderBadge({ provider }: { provider?: string | null }) {
  if (provider !== "mock") return null;
  return <span className="badge sim" title="Produced by the local simulation provider, not by BytePlus">Simulation</span>;
}

export function Thumb({ id, className, onClick, alt, full }: { id: string; className?: string; onClick?: () => void; alt?: string; full?: boolean }) {
  // Lists and nodes show a small preview; `full` asks for the original file.
  const img = <img src={assetUrl(full ? id : `${id}~t`)} decoding="async" alt={alt ?? "Image"} className={className ?? "thumb"} draggable={false} />;
  return onClick ? <button className="thumb-btn" onClick={onClick} aria-label={alt ?? "Open image"}>{img}</button> : img;
}

const n = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

export function planSummary(plan: any): string {
  const images = (plan.byModel as any[]).map((m) => `${n(m.count, "new image", "new images")} × ${m.label}`);
  const parts = images.length ? images : ["No new images"];
  parts.push(plan.newTexts ? n(plan.newTexts, "text request", "text requests") : "no text requests");
  if (plan.reused) parts.push(`${n(plan.reused, "saved result", "saved results")} reused from disk`);
  if (plan.sharedRefs) parts.push("same reference in several branches");
  return parts.join(", ");
}
export const requests = (count: number) => n(count, "request", "requests");

export const SETTING_LABEL: Record<string, string> = { aspect: "Ratio", size: "Quality", output_format: "Format", background: "Background", optimize_prompt: "Prompt optimization" };
export const VALUE_LABEL: Record<string, string> = { auto: "Auto", opaque: "Opaque", transparent: "Transparent", standard: "Standard", fast: "Fast", png: "PNG", jpeg: "JPEG" };

const PATHS: Record<string, string> = {
  plus: "M12 5v14M5 12h14",
  arrow: "M5 12h14M13 6l6 6-6 6",
  clip: "M21 11.5l-8.6 8.6a5.5 5.5 0 01-7.8-7.8l9-9a3.7 3.7 0 015.2 5.2l-9 9a1.8 1.8 0 01-2.6-2.6l8.3-8.3",
  gear: "M12 15.2a3.2 3.2 0 100-6.4 3.2 3.2 0 000 6.4zM19.4 13.5a7.6 7.6 0 000-3l2-1.5-2-3.4-2.3.9a7.6 7.6 0 00-2.6-1.5L14 2.5h-4l-.5 2.5a7.6 7.6 0 00-2.6 1.5l-2.3-.9-2 3.4 2 1.5a7.6 7.6 0 000 3l-2 1.5 2 3.4 2.3-.9a7.6 7.6 0 002.6 1.5l.5 2.5h4l.5-2.5a7.6 7.6 0 002.6-1.5l2.3.9 2-3.4z",
  chat: "M20 12a8 8 0 01-11.8 7L4 20l1.1-4A8 8 0 1120 12z",
  nodes: "M6 4h4v4H6zM14 16h4v4h-4zM14 4h4v4h-4zM8 8v4a4 4 0 004 4h2M16 8v8",
  image: "M4 5h16v14H4zM4 15l4.5-4.5 4 4 2.5-2.5L20 16M9 9.5a1 1 0 100-2 1 1 0 000 2z",
  doc: "M7 3h7l4 4v14H7zM14 3v4h4M10 12h5M10 16h5",
  x: "M6 6l12 12M18 6L6 18",
  user: "M12 12a4 4 0 100-8 4 4 0 000 8zM4.5 20.5a7.5 7.5 0 0115 0",
  history: "M4 6h16M4 12h10M4 18h13",
  trash: "M5 7h14M10 7V4h4v3M7 7l1 13h8l1-13",
  spark: "M12 3v5M12 16v5M3 12h5M16 12h5M6 6l3 3M15 15l3 3M18 6l-3 3M9 15l-3 3",
  undo: "M9 7L4 12l5 5M4 12h10a6 6 0 010 12",
  play: "M8 5l11 7-11 7z",
  pin: "M9 4h6l-1 6 3 3H7l3-3zM12 13v7",
  book: "M5 4h9a4 4 0 014 4v12H9a4 4 0 01-4-4zM5 16a4 4 0 014-4h9",
  check: "M5 12.5l4.5 4.5L19 7.5",
  film: "M4 5h16v14H4zM8 5v14M16 5v14M4 9h4M4 15h4M16 9h4M16 15h4",
  sliders: "M4 7h10M18 7h2M4 17h4M12 17h8M16 5v4M10 15v4",
};
export function Icon({ name, size = 16 }: { name: keyof typeof PATHS | string; size?: number }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d={PATHS[name] ?? ""} />
    </svg>
  );
}
