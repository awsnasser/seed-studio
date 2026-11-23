import { useEffect, useState } from "react";
import { rpc } from "../api";
import { useShell } from "../shared";

type Row = {
  id: number; startedAt: number; finishedAt: number | null; purpose: string; method: string; host: string; path: string; connection: string;
  model: string | null; params: any; outcome: string; httpStatus: number | null; errorCode: string | null; errorMessage: string | null;
  requestId: string | null; taskId: string | null; usage: any; info: string | null;
};
type Log = { rows: Row[]; total: number; totals: { purpose: string; requests: number; ok: number; unknown: number; tokens: number }[]; videoTokens: number; since: number | null };

const PURPOSE: Record<string, string> = {
  chat: "Chat", chat_tools: "Assistant", image: "Image", video_create: "Video", video_status: "Video status check", models: "Model list",
  balance: "Balance", library: "Library list", library_upload: "Library upload", library_asset: "Library check", download: "Result download",
};
const OUTCOME: Record<string, string> = { ok: "OK", partial: "Cut short", error: "Refused", unknown: "Unknown", not_sent: "Not sent", sending: "Sending…" };
const when = (ms: number) => new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });
const num = (n: number) => Math.round(n).toLocaleString("en-US");

/** Settings section: every request the app sent to the provider, and a CSV export of all of them. */
export function ApiLogSettings() {
  const { toast } = useShell();
  const [log, setLog] = useState<Log | null>(null);
  const [paidOnly, setPaidOnly] = useState(true);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    rpc<Log>("api_log_list", { paidOnly, limit: 50 }).then((l) => live && setLog(l)).catch(() => live && setLog(null));
    return () => { live = false; };
  }, [paidOnly]);

  const exportCsv = async () => {
    setBusy(true);
    try {
      const r = await rpc("api_log_export");
      toast(`Saved ${r.rows} requests to ${r.path}`);
    } catch (e) {
      toast((e as Error).message, "error");
    } finally {
      setBusy(false);
    }
  };

  const sent = log?.totals.reduce((a, t) => a + t.requests, 0) ?? 0;
  const unknown = log?.totals.reduce((a, t) => a + t.unknown, 0) ?? 0;
  const tokens = (log?.totals.reduce((a, t) => a + t.tokens, 0) ?? 0) + (log?.videoTokens ?? 0);

  return (
    <section>
      <div className="row between">
        <h3>Request log</h3>
        <button className="btn" onClick={exportCsv} disabled={busy || !log?.since}>{busy ? "Saving…" : "Export all as CSV"}</button>
      </div>
      <p className="muted small">
        Every request this app sends to BytePlus or the gateway is recorded here: time, address, model, options, HTTP status, the provider’s request and task IDs, and the tokens it reported.
        Request bodies and credentials are excluded. Provider error text may contain request details; review logs before sharing. Simulation sends nothing, so it is not listed.
      </p>
      {!log?.since ? (
        <p className="muted small">Nothing has been sent since this log was added. Requests from before it are not in this list; their results are still in your projects.</p>
      ) : (
        <>
          <div className="log-sum">
            <span><b>{num(sent)}</b> generation and upload requests since {when(log.since)}</span>
            <span><b>{num(tokens)}</b> tokens reported</span>
            {unknown > 0 && <span className="warn-text"><b>{unknown}</b> with an unknown outcome</span>}
          </div>
          <div className="segmented" role="radiogroup" aria-label="Which requests to list">
            <button role="radio" aria-checked={paidOnly} className={paidOnly ? "on" : ""} onClick={() => setPaidOnly(true)}>Generation requests</button>
            <button role="radio" aria-checked={!paidOnly} className={!paidOnly ? "on" : ""} onClick={() => setPaidOnly(false)}>All requests</button>
          </div>
          <div className="log-scroll">
            <table className="models log">
              <thead><tr><th>Sent</th><th>Request</th><th>Model</th><th>Result</th><th>Tokens</th><th>Task / request ID</th></tr></thead>
              <tbody>
                {log.rows.map((r) => (
                  <tr key={r.id} title={r.errorMessage ?? `${r.method} ${r.host}${r.path}`}>
                    <td>{when(r.startedAt)}</td>
                    <td>{PURPOSE[r.purpose] ?? r.purpose}{r.params?.resolution ? ` · ${r.params.resolution}` : r.params?.size ? ` · ${r.params.size}` : ""}{r.params?.duration ? ` · ${r.params.duration}s` : ""}{r.info ? ` · ${r.info}` : ""}</td>
                    <td><span className="mono">{r.model ?? ""}</span></td>
                    <td><span className={`badge log-${r.outcome}`}>{OUTCOME[r.outcome] ?? r.outcome}{r.httpStatus && r.outcome !== "ok" ? ` ${r.httpStatus}` : ""}</span>{r.errorCode ? <span className="muted small"> {r.errorCode}</span> : null}</td>
                    <td>{typeof r.usage?.total_tokens === "number" ? num(r.usage.total_tokens) : ""}</td>
                    <td><span className="mono">{r.taskId ?? r.requestId ?? ""}</span></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p className="muted small">
            Showing the newest {log.rows.length} of {num(log.total)}. The CSV has all of them, including status checks and downloads. A video’s tokens arrive with its status check, so they are on that line and not on the request that started it.
            The log holds token counts, not prices: the real charge is what the gateway reports.
          </p>
        </>
      )}
    </section>
  );
}
