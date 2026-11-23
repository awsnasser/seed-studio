import { TeamSettings } from "./Team";
import { ApiLogSettings } from "./ApiLog";
import { useState } from "react";
import { rpc } from "../api";
import { AppState, Icon, STATUS_LABEL, useShell } from "../shared";

export default function SettingsDialog({ onClose }: { onClose: () => void }) {
  const { app, setApp, toast } = useShell();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [check, setCheck] = useState<{ ok: boolean; text: string } | null>(null);
  const [tests, setTests] = useState<Record<string, { ok: boolean; text: string }>>({});

  const set = async (k: string, value: string) => {
    try {
      setApp(await rpc<AppState>("setting_set", { key: k, value }));
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  const runCheck = async () => {
    setBusy("check");
    setCheck(null);
    try {
      const r = await rpc("check_connection");
      setApp(r.state);
      if (r.balance) setCheck({ ok: true, text: `Connected to the gateway: the key is valid. Budget — ${budgetText(r.balance)}.${r.library != null ? ` Character library: ${r.library} images (faces in videos are sent from it by reference).` : ""} The gateway has no model list — use “Test” on a chat model to confirm it answers.` });
      else setCheck({ ok: true, text: `Connected to ${r.region}: the key is valid and ${r.visibleCount} models are listed. Listing is not access — use “Test” on a chat model to confirm it answers.` });
    } catch (e) {
      setCheck({ ok: false, text: (e as Error).message });
    } finally {
      setBusy(null);
    }
  };
  const saveKey = async () => {
    try {
      // The value goes to the local layer once and is never read back by the UI.
      setApp(await rpc<AppState>("key_set", { region: app.region, key }));
      setKey("");
      toast("Key saved in the macOS Keychain.");
      void runCheck();
    } catch (e) {
      toast((e as Error).message, "error");
    }
  };
  const test = async (modelKey: string) => {
    setBusy(modelKey);
    try {
      const r = await rpc("model_test", { modelKey });
      setApp(r.state);
      setTests((t) => ({ ...t, [modelKey]: { ok: r.ok, text: r.ok ? `Answered as ${r.model}.` : r.message } }));
    } catch (e) {
      setTests((t) => ({ ...t, [modelKey]: { ok: false, text: (e as Error).message } }));
    } finally {
      setBusy(null);
    }
  };
  const chat = app.models.filter((m) => m.kind === "chat");
  const image = app.models.filter((m) => m.kind === "image");
  const live = app.providerMode === "modelark";
  const gw = app.gateway;
  const host = (() => { try { return new URL(app.regions[app.region]).host; } catch { return ""; } })();

  return (
    <div className="overlay" role="dialog" aria-label="Settings" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog settings">
        <div className="dialog-head">
          <h2>Settings</h2>
          <button className="round" onClick={onClose} aria-label="Close settings"><Icon name="x" /></button>
        </div>

        <section>
          <h3>Appearance</h3>
          <div className="segmented" role="radiogroup" aria-label="Appearance">
            {[["light", "Light"], ["dark", "Dark"], ["system", "System"]].map(([v, l]) => (
              <button key={v} role="radio" aria-checked={app.theme === v} className={app.theme === v ? "on" : ""} onClick={() => set("theme", v)}>{l}</button>
            ))}
          </div>
        </section>

        <section>
          <h3>Provider</h3>
          <div className="segmented" role="radiogroup" aria-label="Provider">
            <button role="radio" aria-checked={live} className={live ? "on" : ""} onClick={() => set("provider_mode", "modelark")}>BytePlus ModelArk</button>
            <button role="radio" aria-checked={!live} className={!live ? "on" : ""} onClick={() => set("provider_mode", "mock")}>Local simulation</button>
          </div>
          <p className="muted small">Simulation never touches the network and labels its results. The app never changes provider, region or model on its own after an error.</p>
        </section>

        <section className={live ? "" : "dim"}>
          <h3>ModelArk connection</h3>
          <ol className="steps">
            <li>
              <span className="step-n">1</span>
              <div className="grow">
                <div className="field-row">
                  <label htmlFor="region">Connection</label>
                  <select id="region" value={app.region} onChange={(e) => { setCheck(null); void set("region", e.target.value); }}>
                    {Object.keys(app.regions).map((r) => <option key={r} value={r}>{app.gateways?.[r] ? `${app.gateways[r].label} (budgeted)` : `BytePlus direct · ${r}`}</option>)}
                  </select>
                  <span className={`badge ${app.hasKey[app.region] ? "ok" : "warnb"}`}>{app.hasKey[app.region] ? "Key saved" : "No key"}</span>
                </div>
                <p className="muted small">{gw
                  ? <>Uses the gateway’s own key, not a BytePlus console key. Your key, prompts and images go to <span className="mono">{host}</span>, which forwards them to BytePlus ModelArk and charges a fixed budget. Chat replies are capped at {gw.maxCompletionTokens} tokens and videos always send an explicit length, to keep budget holds small.</>
                  : "Use the region where the key was created. Keys are not shared between connections."}</p>
                <div className="prices">
                  <b>Your prices (optional)</b>
                  <label>USD per million video tokens <input inputMode="decimal" defaultValue={app.prices?.videoPerM ?? ""} placeholder="e.g. 2.5" onBlur={(e) => set("price_video_per_m", e.target.value.trim())} aria-label="USD per million video tokens" /></label>
                  <label>USD per image <input inputMode="decimal" defaultValue={app.prices?.imageEach ?? ""} placeholder="e.g. 0.04" onBlur={(e) => set("price_image_each", e.target.value.trim())} aria-label="USD per image" /></label>
                  <span className="muted small">The app always shows the tokens each video and image used (reported by BytePlus). Enter your prices to also see an estimate in dollars; they are only used for display. Check your provider account for actual charges.</span>
                </div>
                {gw?.assetsUrl && (
                  <div className={`attest ${app.libraryUpload ? "on" : ""}`}>
                    <div className="attest-head">
                      <b>My reference images are AI-generated: add new ones to the gateway library</b>
                      <div className="segmented" role="radiogroup" aria-label="Add new reference images to the gateway library">
                        <button role="radio" aria-checked={!app.libraryUpload} className={!app.libraryUpload ? "on" : ""} onClick={() => set(`library_upload:${app.region}`, "0")}>Off</button>
                        <button role="radio" aria-checked={app.libraryUpload} className={app.libraryUpload ? "on" : ""} onClick={() => set(`library_upload:${app.region}`, "1")}>On</button>
                      </div>
                    </div>
                    <span className="muted small">Seedance refuses a face that is not in the gateway’s character library. With this on, an image a video uses that is not there yet is uploaded to the library first, with your declaration that it is AI-generated and shows no real person (the gateway logs each declaration and has given BytePlus that undertaking). Leave it off if any of your reference images shows a real person. To keep one image out, select its node and choose “Don’t use — send the file”.</span>
                  </div>
                )}
              </div>
            </li>
            <li>
              <span className="step-n">2</span>
              <div className="grow">
                <div className="field-row">
                  <input id="apikey" type="password" autoComplete="off" spellCheck={false} value={key} onChange={(e) => setKey(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && key.trim() && saveKey()} placeholder={app.hasKey[app.region] ? "Paste a new key to replace the saved one" : "Paste your API key"} aria-label="API key" />
                  <button className="btn primary" disabled={!key.trim()} onClick={saveKey}>Save & check</button>
                  {app.hasKey[app.region] && <button className="btn" onClick={async () => { setCheck(null); setApp(await rpc<AppState>("key_delete", { region: app.region })); }}>Remove</button>}
                </div>
                <p className="muted small">Stored only in the macOS Keychain. It is never shown again or written to the database or logs.</p>
              </div>
            </li>
            <li>
              <span className="step-n">3</span>
              <div className="grow">
                <div className="row">
                  <button className="btn" onClick={runCheck} disabled={busy !== null || !live || !app.hasKey[app.region]}>{busy === "check" ? <><span className="spinner" /> Checking…</> : "Check connection"}</button>
                  <span className="muted small">{gw ? "Reads the gateway balance only." : "Lists models only."} No prompt is sent and nothing is generated.</span>
                </div>
                {check && <div className={`result ${check.ok ? "ok" : "bad"}`}>{check.text}</div>}
              </div>
            </li>
          </ol>
        </section>

        <TeamSettings />

        <section>
          <h3>Models</h3>
          <div className="field-row">
            <label htmlFor="chatm">Chat & canvas assistant</label>
            <select id="chatm" value={app.chatModel} onChange={(e) => set("chat_model", e.target.value)}>
              {chat.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}
            </select>
            <label htmlFor="think">Thinking</label>
            <select id="think" value={app.thinking} onChange={(e) => set("thinking", e.target.value)}>
              <option value="default">Model default</option><option value="enabled">On</option><option value="disabled">Off</option>
            </select>
          </div>
          <div className="field-row">
            <label htmlFor="imgm">Default image model for new nodes</label>
            <select id="imgm" value={app.imageModel} onChange={(e) => set("image_model", e.target.value)}>
              {image.map((m) => <option key={m.key} value={m.key}>{m.label}</option>)}
            </select>
          </div>
          <table className="models">
            <thead><tr><th>Model</th><th>ID sent</th><th>Status in {app.region}</th><th>Custom endpoint ID</th><th /></tr></thead>
            <tbody>
              {app.models.map((m) => (
                <tr key={`${m.key}-${app.region}`}>
                  <td>{m.label}</td>
                  <td><span className="mono">{m.resolvedId}</span></td>
                  <td><span className={`badge st-${m.status}`}>{STATUS_LABEL[m.status] ?? m.status}</span></td>
                  <td><input className="mono" defaultValue={m.endpoint} disabled={gw?.acceptsEndpointIds === false} title={gw?.acceptsEndpointIds === false ? "This gateway refuses endpoint IDs; the model ID is sent." : undefined} placeholder="ep-… (optional)" aria-label={`Endpoint ID for ${m.label}`}
                    onBlur={(e) => e.target.value.trim() !== m.endpoint && set(`endpoint:${m.key}`, e.target.value.trim())} /></td>
                  <td>{m.kind === "chat" && <button className="btn small" disabled={busy !== null || !live || !app.hasKey[app.region]} onClick={() => test(m.key)} title="Sends one tiny real chat request">{busy === m.key ? "Testing…" : "Test"}</button>}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {Object.entries(tests).map(([k, t]) => (
            <div key={k} className={`result ${t.ok ? "ok" : "bad"}`}><b>{app.models.find((m) => m.key === k)?.label}:</b> {t.text}</div>
          ))}
          <p className="muted small">
            “Listed” means the ID appears in the account’s model list. “Working” is recorded only after a real request you sent succeeded. “No access” means BytePlus refused that model for this key:
            activate it under Model activation in the <a href="https://console.byteplus.com/ark" target="_blank" rel="noreferrer">BytePlus console</a>, choose another model, or enter the endpoint ID that the console gives you.
            Image models are not tested here because a test would generate a paid image.
          </p>
        </section>

        <ApiLogSettings />
      </div>
    </div>
  );
}

/** The gateway's balance, whatever its exact field names: every number it reports. */
function budgetText(b: any): string {
  const out: string[] = [];
  const walk = (o: any, prefix: string) => {
    for (const [k, v] of Object.entries(o ?? {})) {
      if (typeof v === "number" || typeof v === "string") out.push(`${prefix}${k.replace(/_/g, " ")}: ${v}`);
      else if (v && typeof v === "object" && !Array.isArray(v) && !prefix) walk(v, `${k} `);
    }
  };
  walk(b, "");
  return out.slice(0, 8).join(" · ") || "reported";
}
