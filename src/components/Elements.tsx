import { useCallback, useEffect, useState } from "react";
import { rpc } from "../api";
import { Icon, Project, Thumb, useShell } from "../shared";
import { importImage, useAssets } from "./Gallery";

export type Element = { id: string; kind: "character" | "environment" | "prop"; name: string; handle: string; assetId: string | null; description: string };
const KINDS: [Element["kind"], string, string, string][] = [
  ["character", "Characters", "character", "A person or creature that must look the same in every shot."],
  ["environment", "Environments", "environment", "A place: its look, light and mood."],
  ["prop", "Props", "prop", "An object or product that must stay identical."],
];

/** The project's elements. Reading them sends nothing. */
export function useElements(projectId: string) {
  const [elements, setElements] = useState<Element[]>([]);
  const reload = useCallback(async () => setElements(await rpc<Element[]>("elements_list", { projectId })), [projectId]);
  useEffect(() => { void reload().catch(() => {}); }, [reload]);
  return { elements, setElements, reload };
}

export default function ElementsView({ project }: { project: Project }) {
  const { toast, viewAsset } = useShell();
  const { elements, setElements } = useElements(project.id);
  const [editing, setEditing] = useState<Partial<Element> | null>(null);
  const copy = (e: Element) => { void navigator.clipboard?.writeText(`@${e.handle}`); toast(`Copied @${e.handle}. Paste it in any prompt, or just type @ there.`); };
  const remove = async (e: Element) => {
    try { setElements(await rpc("element_delete", { projectId: project.id, id: e.id })); toast(`Removed ${e.name}. Prompts that say @${e.handle} now send it as plain text.`); }
    catch (err) { toast((err as Error).message, "error"); }
  };
  return (
    <div className="elements">
      <header className="elements-head">
        <h2>Elements</h2>
        <p className="muted">Lock a character, a place or a prop once — a name, one image and what must never change. Then call it in any prompt on the canvas by typing <b>@</b> and its name: its image is sent as a reference and its description is added for you.</p>
      </header>
      {KINDS.map(([kind, title, one, hint]) => {
        const list = elements.filter((e) => e.kind === kind);
        return (
          <section key={kind} className="elements-section">
            <div className="elements-title">
              <h3>{title} <span className="count">{list.length}</span></h3>
              <button className="btn small primary" onClick={() => setEditing({ kind, name: "", description: "", assetId: null })}><Icon name="plus" size={13} />Add {one}</button>
            </div>
            {list.length === 0
              ? <p className="muted small">{hint}</p>
              : (
                <div className="element-grid">
                  {list.map((e) => (
                    <article key={e.id} className="element-card">
                      {e.assetId
                        ? <Thumb id={e.assetId} className="element-img" onClick={() => viewAsset(e.assetId)} alt={e.name} />
                        : <div className="element-img empty"><Icon name="image" size={20} /><span>No image</span></div>}
                      <div className="element-body">
                        <b dir="auto">{e.name}</b>
                        <button className="handle" onClick={() => copy(e)} title="Copy to paste in a prompt">@{e.handle}</button>
                        {e.description && <p dir="auto">{e.description}</p>}
                        <div className="row">
                          <button className="btn small" onClick={() => setEditing(e)}>Edit</button>
                          <button className="btn small" onClick={() => remove(e)}>Remove</button>
                        </div>
                      </div>
                    </article>
                  ))}
                </div>
              )}
          </section>
        );
      })}
      {editing && <ElementEditor projectId={project.id} value={editing} onClose={(list) => { if (list) setElements(list); setEditing(null); }} />}
    </div>
  );
}

function ElementEditor({ projectId, value, onClose }: { projectId: string; value: Partial<Element>; onClose: (list?: Element[]) => void }) {
  const { toast } = useShell();
  const { assets, reload } = useAssets(projectId);
  const [name, setName] = useState(value.name ?? "");
  const [kind, setKind] = useState<Element["kind"]>(value.kind ?? "character");
  const [assetId, setAssetId] = useState<string | null>(value.assetId ?? null);
  const [description, setDescription] = useState(value.description ?? "");
  const [busy, setBusy] = useState(false);
  const handle = name.replace(/[^\p{L}\p{N}_]/gu, "");
  const save = async () => {
    setBusy(true);
    try { onClose(await rpc<Element[]>("element_save", { projectId, id: value.id, kind, name, assetId, description })); }
    catch (e) { toast((e as Error).message, "error"); setBusy(false); }
  };
  return (
    <div className="overlay" role="dialog" aria-label="Element">
      <div className="dialog element-editor">
        <div className="dialog-head">
          <h2>{value.id ? "Edit element" : "New element"}</h2>
          <button className="round" onClick={() => onClose()} aria-label="Close"><Icon name="x" /></button>
        </div>
        <div className="segmented" role="radiogroup" aria-label="Kind">
          {KINDS.map(([k, , one]) => <button key={k} role="radio" aria-checked={kind === k} className={kind === k ? "on" : ""} onClick={() => setKind(k)}>{one[0].toUpperCase() + one.slice(1)}</button>)}
        </div>
        <label className="field"><span>Name</span>
          <input dir="auto" value={name} maxLength={40} onChange={(e) => setName(e.target.value)} placeholder="e.g. Suhail, Red Tent, Dallah" autoFocus />
        </label>
        <p className="muted small">{handle ? <>You call it as <b>@{handle}</b>.</> : "The name is what you type after @."}</p>
        <div className="field"><span>Image</span>
          <div className="element-pick">
            {assetId ? <Thumb id={assetId} className="element-img" alt="Chosen image" /> : <div className="element-img empty"><Icon name="image" size={20} /><span>No image yet</span></div>}
            <div className="col">
              <button className="btn" onClick={async () => { try { const a = await importImage(projectId); if (a) { setAssetId(a.id); void reload(); } } catch (e) { toast((e as Error).message, "error"); } }}>Choose from this Mac…</button>
              {assetId && <button className="btn small" onClick={() => setAssetId(null)}>Remove image</button>}
              <span className="muted small">One clear picture. It is sent as a reference whenever you call the element.</span>
            </div>
          </div>
          {assets.length > 0 && <>
            <span className="field-label">Or pick from this project</span>
            <div className="pick-grid scroll">{assets.slice(0, 60).map((a) => <Thumb key={a.id} id={a.id} className={`thumb small ${a.id === assetId ? "picked" : ""}`} onClick={() => setAssetId(a.id)} alt={a.title} />)}</div>
          </>}
        </div>
        <label className="field"><span>What must never change</span>
          <textarea dir="auto" rows={4} value={description} maxLength={1200} onChange={(e) => setDescription(e.target.value)}
            placeholder={kind === "character" ? "e.g. Emirati man in his late twenties, short black beard, white ghutra and white kandura" : kind === "environment" ? "e.g. Bedouin tent with a red patterned rug, warm lantern light, blue-hour sky" : "e.g. brass dallah coffee pot with a curved spout"} />
        </label>
        <p className="muted small">This text is added to every prompt that calls the element, so the look stays locked.</p>
        <div className="row end">
          <button className="btn" onClick={() => onClose()}>Cancel</button>
          <button className="btn primary" disabled={busy || !handle} onClick={save}>{value.id ? "Save" : "Add element"}</button>
        </div>
      </div>
    </div>
  );
}
