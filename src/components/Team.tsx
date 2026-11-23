import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { rpc } from "../api";
import { Icon, useShell } from "../shared";

/** Team: the person signed in on this Mac, the people with accounts, and shared sessions (8 seats each). */
export type CloudUser = { id: string; email: string; name: string; role: "admin" | "user" };
export type CloudState = { configured: boolean; url: string; user: CloudUser | null };
type Person = CloudUser & { disabled: boolean };
type Session = { id: string; name: string; owner: string; workspace_members: { user_id: string; role: string }[] };
export const SEATS = 8;

const EMPTY: CloudState = { configured: false, url: "", user: null };
const CHANGED = "cloud-changed";
/** The team state. Reading it asks nobody: it is what this Mac already knows. */
export function useCloud() {
  const [cloud, setCloud] = useState<CloudState>(EMPTY);
  const reload = useCallback(async () => { try { setCloud(await rpc<CloudState>("cloud_state")); } catch { /* stays as it was */ } }, []);
  useEffect(() => {
    void reload();
    const on = (e: Event) => setCloud((e as CustomEvent<CloudState>).detail);
    window.addEventListener(CHANGED, on);
    return () => window.removeEventListener(CHANGED, on);
  }, [reload]);
  const publish = (next: CloudState) => window.dispatchEvent(new CustomEvent(CHANGED, { detail: next }));
  return { cloud, publish };
}

/** A temporary password an admin passes on once. Letters that are easy to tell apart. */
function tempPassword() {
  const abc = "abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  return Array.from(crypto.getRandomValues(new Uint32Array(16)), (n) => abc[n % abc.length]).join("").replace(/(.{4})(?=.)/g, "$1-");
}

export function ProfileButton({ onSettings }: { onSettings: () => void }) {
  const { toast } = useShell();
  const { cloud, publish } = useCloud();
  const [open, setOpen] = useState(false);
  const [team, setTeam] = useState(false);
  const [busy, setBusy] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [name, setName] = useState("");
  const [newPass, setNewPass] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  const me = cloud.user;
  useEffect(() => setName(me?.name ?? ""), [me?.name]);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    window.addEventListener("pointerdown", close, true);
    return () => window.removeEventListener("pointerdown", close, true);
  }, [open]);
  const act = async (fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    try { await fn(); } catch (e) { toast((e as Error).message, "error"); } finally { setBusy(false); }
  };
  const signIn = () => act(async () => {
    publish(await rpc<CloudState>("cloud_sign_in", { email, password }));
    setPassword("");
    toast("Signed in.");
  });
  const initial = (me?.name || me?.email || "").trim().charAt(0).toUpperCase();
  return (
    <div className="profile" ref={box}>
      <button className={`round ${me ? "signed" : ""}`} onClick={() => setOpen((o) => !o)} title={me ? `Signed in as ${me.name || me.email}` : "Your account"} aria-label="Your account" aria-expanded={open}>
        {initial ? <b className="profile-initial" dir="auto">{initial}</b> : <Icon name="user" />}
      </button>
      {open && (
        <div className="profile-menu" role="dialog" aria-label="Your account">
          {!cloud.configured ? (
            <>
              <b>Team</b>
              <p className="muted small">Shared sessions need the team connection. Add your Supabase project’s address once in Settings → Team, then sign in here.</p>
              <button className="btn" onClick={() => { setOpen(false); onSettings(); }}>Open Settings</button>
            </>
          ) : !me ? (
            <form className="col" onSubmit={(e) => { e.preventDefault(); void signIn(); }}>
              <b>Sign in to your team</b>
              <label className="field"><span>Email</span><input type="email" autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} autoFocus /></label>
              <label className="field"><span>Password</span><input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} /></label>
              <button className="btn primary" disabled={busy || !email || !password}>{busy ? "Signing in…" : "Sign in"}</button>
              <p className="muted small">Accounts are created by an admin. Your password goes only to the team server and is not kept on this Mac; the sign-in itself is kept in the Keychain.</p>
            </form>
          ) : (
            <>
              <label className="field"><span>Your name</span>
                <input dir="auto" value={name} maxLength={60} onChange={(e) => setName(e.target.value)}
                  onBlur={() => name.trim() !== me.name && act(async () => { publish(await rpc<CloudState>("cloud_rename_me", { name })); })} />
              </label>
              <div className="profile-id"><span dir="ltr">{me.email}</span><span className={`badge ${me.role === "admin" ? "ok" : ""}`}>{me.role === "admin" ? "Admin" : "User"}</span></div>
              <button className="btn primary" onClick={() => { setOpen(false); setTeam(true); }}>{me.role === "admin" ? "Shared sessions and people…" : "Shared sessions…"}</button>
              {newPass === null
                ? <button className="btn" onClick={() => setNewPass("")}>Change password</button>
                : (
                  <form className="col" onSubmit={(e) => { e.preventDefault(); void act(async () => { await rpc("cloud_change_password", { password: newPass }); setNewPass(null); toast("Password changed."); }); }}>
                    <label className="field"><span>New password (10 characters or more)</span><input type="password" autoComplete="new-password" value={newPass} onChange={(e) => setNewPass(e.target.value)} autoFocus /></label>
                    <div className="row"><button className="btn primary" disabled={busy || newPass.length < 10}>Save</button><button type="button" className="btn" onClick={() => setNewPass(null)}>Cancel</button></div>
                  </form>
                )}
              <button className="btn" disabled={busy} onClick={() => act(async () => { publish(await rpc<CloudState>("cloud_sign_out")); toast("Signed out on this Mac."); })}>Sign out</button>
            </>
          )}
        </div>
      )}
      {team && me && <TeamDialog me={me} onClose={() => setTeam(false)} onState={publish} />}
    </div>
  );
}

function TeamDialog({ me, onClose, onState }: { me: CloudUser; onClose: () => void; onState: (s: CloudState) => void }) {
  const { toast } = useShell();
  const [tab, setTab] = useState<"sessions" | "people">("sessions");
  const [people, setPeople] = useState<Person[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [newSession, setNewSession] = useState("");
  const [draft, setDraft] = useState({ email: "", name: "", role: "user", password: tempPassword() });
  const [shown, setShown] = useState<{ title: string; email: string; password: string } | null>(null);
  const admin = me.role === "admin";
  const going = useRef(false);
  /** One request at a time; a second click never sends a second one. */
  const act = async (fn: () => Promise<void>) => {
    if (going.current) return;
    going.current = true;
    setBusy(true);
    try { await fn(); } catch (e) { toast((e as Error).message, "error"); } finally { going.current = false; setBusy(false); }
  };
  const load = useCallback(async () => {
    try {
      const p = await rpc<{ users: Person[]; state: CloudState }>("cloud_people");
      setPeople(p.users);
      onState(p.state);
      setSessions(await rpc<Session[]>("cloud_sessions"));
    } catch (e) { toast((e as Error).message, "error"); } finally { setLoading(false); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => { void load(); }, [load]);
  const who = (id: string) => people.find((p) => p.id === id);
  const label = (id: string) => { const p = who(id); return p ? p.name || p.email : "Someone"; };
  const adminCall = (payload: Record<string, unknown>) => rpc<{ user?: Person }>("cloud_admin", payload);
  const replace = (u?: Person) => { if (u) setPeople((ps) => (ps.some((p) => p.id === u.id) ? ps.map((p) => (p.id === u.id ? u : p)) : [...ps, u])); };
  const create = () => act(async () => {
    const r = await adminCall({ action: "create_user", ...draft });
    replace(r.user);
    setShown({ title: "Account created", email: draft.email.trim(), password: draft.password });
    setDraft({ email: "", name: "", role: "user", password: tempPassword() });
  });
  const reset = (p: Person) => act(async () => {
    const password = tempPassword();
    await adminCall({ action: "set_password", userId: p.id, password });
    setShown({ title: "Password reset", email: p.email, password });
  });

  return createPortal(
    <div className="overlay" role="dialog" aria-label="Team" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog team">
        <div className="dialog-head">
          <h2>Team</h2>
          <button className="round" onClick={onClose} aria-label="Close"><Icon name="x" /></button>
        </div>
        {admin && (
          <div className="segmented" role="tablist">
            <button role="tab" aria-selected={tab === "sessions"} className={tab === "sessions" ? "on" : ""} onClick={() => setTab("sessions")}>Shared sessions</button>
            <button role="tab" aria-selected={tab === "people"} className={tab === "people" ? "on" : ""} onClick={() => setTab("people")}>People · {people.length}</button>
          </div>
        )}
        {loading ? <div className="boot"><span className="spinner" /></div> : tab === "sessions" ? (
          <>
            <p className="muted">A shared session is a group of up to {SEATS} people who will work on the same project. Sharing a project into a session comes in the next step; for now you can set up who is in each one.</p>
            <form className="row" onSubmit={(e) => { e.preventDefault(); void act(async () => { setSessions(await rpc<Session[]>("cloud_session_create", { name: newSession })); setNewSession(""); }); }}>
              <input className="grow" dir="auto" value={newSession} maxLength={80} placeholder="New session name, e.g. Suhail film" onChange={(e) => setNewSession(e.target.value)} />
              <button className="btn primary" disabled={busy || !newSession.trim()}>Create session</button>
            </form>
            {sessions.length === 0 && <p className="muted">No shared sessions yet.</p>}
            {sessions.map((s) => {
              const mine = s.owner === me.id || admin;
              const ids = s.workspace_members.map((m) => m.user_id);
              const free = people.filter((p) => !p.disabled && !ids.includes(p.id));
              return (
                <section key={s.id} className="team-card">
                  <div className="team-card-head">
                    <b dir="auto">{s.name}</b>
                    <span className={`badge ${ids.length >= SEATS ? "warnb" : ""}`}>{ids.length} of {SEATS} seats</span>
                    <span className="grow" />
                    {mine
                      ? <button className="btn small" disabled={busy} onClick={() => window.confirm(`Delete the shared session “${s.name}”? Its members lose access to it.`) && act(async () => setSessions(await rpc<Session[]>("cloud_session_delete", { sessionId: s.id })))}>Delete</button>
                      : <button className="btn small" disabled={busy} onClick={() => act(async () => setSessions(await rpc<Session[]>("cloud_member_remove", { sessionId: s.id, userId: me.id })))}>Leave</button>}
                  </div>
                  <div className="team-members">
                    {s.workspace_members.map((m) => (
                      <span key={m.user_id} className="member" dir="auto">
                        {label(m.user_id)}{m.role === "owner" && <small>owner</small>}
                        {mine && m.role !== "owner" && <button aria-label={`Remove ${label(m.user_id)}`} disabled={busy} onClick={() => act(async () => setSessions(await rpc<Session[]>("cloud_member_remove", { sessionId: s.id, userId: m.user_id })))}>×</button>}
                      </span>
                    ))}
                  </div>
                  {mine && (ids.length >= SEATS
                    ? <p className="muted small">All {SEATS} seats are taken. Remove someone to add another person.</p>
                    : free.length > 0 && (
                      <select value="" disabled={busy} onChange={(e) => e.target.value && act(async () => setSessions(await rpc<Session[]>("cloud_member_add", { sessionId: s.id, userId: e.target.value })))} aria-label="Add a person">
                        <option value="">Add a person…</option>
                        {free.map((p) => <option key={p.id} value={p.id}>{p.name ? `${p.name} · ${p.email}` : p.email}</option>)}
                      </select>
                    ))}
                </section>
              );
            })}
          </>
        ) : (
          <>
            <p className="muted">Only admins create accounts; nobody can sign up on their own. You pass the temporary password on yourself, and the person changes it after signing in.</p>
            <section className="team-card">
              <b>New account</b>
              <div className="team-new">
                <label className="field"><span>Email</span><input type="email" dir="ltr" value={draft.email} onChange={(e) => setDraft({ ...draft, email: e.target.value })} /></label>
                <label className="field"><span>Name</span><input dir="auto" value={draft.name} maxLength={60} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
                <label className="field"><span>Role</span>
                  <select value={draft.role} onChange={(e) => setDraft({ ...draft, role: e.target.value })}><option value="user">User</option><option value="admin">Admin</option></select>
                </label>
              </div>
              <div className="row">
                <button className="btn primary" disabled={busy || !/^\S+@\S+\.\S+$/.test(draft.email.trim())} onClick={create}>Create account</button>
                <span className="muted small">A temporary password is made for you and shown once.</span>
              </div>
            </section>
            <div className="people">
              {people.map((p) => (
                <div key={p.id} className={`person ${p.disabled ? "off" : ""}`}>
                  <div className="grow">
                    <b dir="auto">{p.name || "No name"}</b>{p.id === me.id && <small> · you</small>}
                    <div className="muted small" dir="ltr">{p.email}</div>
                  </div>
                  <span className={`badge ${p.disabled ? "err" : p.role === "admin" ? "ok" : ""}`}>{p.disabled ? "Disabled" : p.role === "admin" ? "Admin" : "User"}</span>
                  <button className="btn small" disabled={busy} onClick={() => reset(p)}>Reset password</button>
                  {p.id !== me.id && <>
                    <button className="btn small" disabled={busy} onClick={() => act(async () => replace((await adminCall({ action: "set_role", userId: p.id, role: p.role === "admin" ? "user" : "admin" })).user))}>{p.role === "admin" ? "Make user" : "Make admin"}</button>
                    <button className="btn small" disabled={busy} onClick={() => act(async () => replace((await adminCall({ action: "set_disabled", userId: p.id, disabled: !p.disabled })).user))}>{p.disabled ? "Enable" : "Disable"}</button>
                  </>}
                </div>
              ))}
            </div>
          </>
        )}
        {shown && (
          <div className="overlay inner" role="alertdialog" aria-label={shown.title}>
            <div className="dialog pass-once">
              <h2>{shown.title}</h2>
              <p className="muted">Send these to the person yourself. The password is shown only now and is not kept anywhere in the app.</p>
              <div className="pass-box"><span dir="ltr">{shown.email}</span><b dir="ltr" className="mono">{shown.password}</b></div>
              <div className="row end">
                <button className="btn" onClick={() => { void navigator.clipboard?.writeText(`${shown.email}\n${shown.password}`); toast("Copied."); }}>Copy both</button>
                <button className="btn primary" onClick={() => setShown(null)}>Done</button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>,
    document.body,
  );
}

/** Settings → Team: the project address and its publishable key. */
export function TeamSettings() {
  const { toast } = useShell();
  const { cloud, publish } = useCloud();
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => setUrl(cloud.url), [cloud.url]);
  const save = async (u: string, k: string) => {
    setBusy(true);
    try {
      publish(await rpc<CloudState>("cloud_configure", { url: u, key: k }));
      setKey("");
      toast(u ? "Team connection saved. Sign in from the person icon at the top." : "Team connection removed.");
    } catch (e) { toast((e as Error).message, "error"); } finally { setBusy(false); }
  };
  return (
    <section>
      <h3>Team</h3>
      <p className="muted small">For shared sessions. Your Supabase project’s address and its <b>publishable</b> (anon) key, from Project Settings → API. Both are public values. Never paste the secret service key here; the app refuses it.</p>
      <label className="field"><span>Project address</span><input dir="ltr" value={url} placeholder="https://abcd1234.supabase.co" onChange={(e) => setUrl(e.target.value)} /></label>
      <label className="field"><span>Publishable key{cloud.configured ? " (saved — paste again only to replace it)" : ""}</span><input dir="ltr" value={key} placeholder={cloud.configured ? "••••••••" : "eyJ… or sb_publishable_…"} onChange={(e) => setKey(e.target.value)} /></label>
      <div className="row">
        <button className="btn primary" disabled={busy || !url.trim() || !key.trim()} onClick={() => save(url, key)}>{busy ? "Checking…" : "Save and check"}</button>
        {cloud.configured && <button className="btn" disabled={busy} onClick={() => window.confirm("Remove the team connection from this Mac? You will be signed out. Nothing online is deleted.") && save("", "")}>Remove</button>}
        <span className="muted small">{cloud.configured ? (cloud.user ? `Connected · signed in as ${cloud.user.name || cloud.user.email}` : "Connected · not signed in") : "Not set up"}</span>
      </div>
    </section>
  );
}
