import { useCallback, useEffect, useRef, useState } from "react";
import { assetUrl, fileToBase64, pickFile, rpc } from "../api";
import { Icon, useShell } from "../shared";

export type VoiceTrack = { id: string; name: string; durationMs: number; peakMs: number; peaks: number[] };
const fmt = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;

async function pickAndImport(projectId: string): Promise<VoiceTrack | null> {
  const file = await pickFile("audio/*,.mp3,.wav,.m4a");
  if (!file) return null;
  return rpc("audio_import", { projectId, name: file.name, base64: await fileToBase64(file) });
}

/** One audio file of the project (by id), or its newest when no id is given. */
export function useVoice(projectId: string, audioId?: string) {
  const { toast } = useShell();
  const [track, setTrack] = useState<VoiceTrack | null>(null);
  const reload = useCallback(async () => setTrack(await rpc("audio_get", { projectId, audioId: audioId || undefined })), [projectId, audioId]);
  useEffect(() => { void reload(); }, [reload]);
  const importTrack = async (): Promise<VoiceTrack | null> => {
    try {
      const t = await pickAndImport(projectId);
      if (t) { setTrack(t); toast("Audio imported. It stays on this Mac until a clip is sent with a video."); }
      return t;
    } catch (e) { toast((e as Error).message, "error"); return null; }
  };
  return { track, reload, importTrack };
}

/** Waveform with one clip: click moves the nearest edge; Play checks what the clip holds. */
function ClipPicker({ track, start, end, active, onRange }: { track: VoiceTrack; start: number; end: number; active: boolean; onRange: (start: number, end: number) => void }) {
  const audio = useRef<HTMLAudioElement | null>(null);
  const [playing, setPlaying] = useState(false);
  const total = track.durationMs / 1000;
  const hasClip = end > start;
  const from = Math.max(0, (hasClip ? start : 0) - 3), to = Math.min(total || 1, (hasClip ? end : 12) + 3);
  const W = 300, H = 46;
  const stop = () => { audio.current?.pause(); setPlaying(false); };
  useEffect(() => stop, []);
  const play = () => {
    if (playing) return stop();
    const a = audio.current ?? (audio.current = new Audio());
    if (!a.src.endsWith(track.id)) a.src = assetUrl(track.id);
    const until = hasClip ? end : to;
    a.currentTime = hasClip ? start : from;
    a.ontimeupdate = () => { if (a.currentTime >= until) stop(); };
    a.onended = stop;
    void a.play().then(() => setPlaying(true)).catch(() => setPlaying(false));
  };
  const clickWave = (e: React.MouseEvent<SVGSVGElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const t = Math.round((from + ((e.clientX - r.left) / r.width) * (to - from)) * 20) / 20;
    if (!hasClip) return onRange(t, Math.min(total, t + 3));
    if (Math.abs(t - start) <= Math.abs(t - end)) onRange(Math.min(t, end - 0.1), end);
    else onRange(start, Math.max(t, start + 0.1));
  };
  const bars = track.peaks.slice(Math.floor((from * 1000) / track.peakMs), Math.ceil((to * 1000) / track.peakMs));
  const max = Math.max(60, ...bars);
  const x = (t: number) => ((t - from) / (to - from)) * W;
  return (
    <>
      <svg className="wave" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" onClick={clickWave} role="img" aria-label="Waveform — click to move the nearest clip edge">
        {hasClip && <rect className={`wave-clip ${active ? "on" : ""}`} x={x(start)} y={0} width={Math.max(1, x(end) - x(start))} height={H} rx={3} />}
        {bars.map((p, i) => { const h = Math.max(1, (p / max) * (H - 4)); return <rect key={i} className="wave-bar" x={(i / bars.length) * W} y={(H - h) / 2} width={Math.max(0.6, W / bars.length - 0.6)} height={h} />; })}
      </svg>
      <div className="voice-row">
        <label>From <input type="number" step={0.1} min={0} max={total} value={start} aria-label="Clip start (seconds)" onChange={(e) => onRange(Number(e.target.value), end)} /></label>
        <label>to <input type="number" step={0.1} min={0} max={total} value={end} aria-label="Clip end (seconds)" onChange={(e) => onRange(start, Number(e.target.value))} /></label>
        <span className="muted">{hasClip ? `${(end - start).toFixed(1)}s` : "no clip"}</span>
        <button className="btn small" onClick={play}>{playing ? "Stop" : "▶ Play"}</button>
      </div>
      {hasClip && end - start > 30 && <div className="warn">A clip sent with a video can be at most 30 seconds.</div>}
    </>
  );
}

/** Voice settings kept on a video node itself (when no audio node is connected). */
export function VoiceSection({ projectId, s, setS }: { projectId: string; s: Record<string, string>; setS: (patch: Record<string, string>) => void }) {
  const { track, importTrack } = useVoice(projectId);
  const on = s.voice === "on";
  return (
    <div className="voice">
      <div className="field"><span>Voice (lip sync)</span>
        <div className="segmented small" role="radiogroup" aria-label="Voice">
          <button role="radio" aria-checked={!on} className={!on ? "on" : ""} onClick={() => setS({ voice: "off" })}>Off</button>
          <button role="radio" aria-checked={on} className={on ? "on" : ""} onClick={() => setS({ voice: "on" })}>Speak this clip</button>
        </div>
      </div>
      {!track && <div className="voice-empty"><span className="muted small">No voice track in this project yet. Import one here, or connect an Audio clip node to the “audio” input.</span><button className="btn small" onClick={() => void importTrack()}>Import voice…</button></div>}
      {track && (
        <>
          <div className="voice-track"><Icon name="film" size={12} /><span className="grow" title={track.name}>{track.name}</span><span className="muted">{fmt(track.durationMs / 1000)}</span>
            <button className="link" onClick={() => void importTrack()}>Replace</button></div>
          <ClipPicker track={track} start={Number(s.voiceStart || 0)} end={Number(s.voiceEnd || 0)} active={on} onRange={(a, b) => setS({ voiceStart: String(a), voiceEnd: String(b) })} />
          <p className="muted small">{on
            ? "This clip is sent with the request and the character is asked to speak it with matching lips. Play it to check it holds exactly this shot’s words."
            : "Off: the clip is only a guide for this shot’s length; no audio is sent. Lay the voice over in your edit."}</p>
        </>
      )}
      <div className="field"><span>Scene sound</span>
        <div className="segmented small" role="radiogroup" aria-label="Scene sound">
          <button role="radio" aria-checked={s.sound !== "off"} className={s.sound !== "off" ? "on" : ""} onClick={() => setS({ sound: "on" })}>Generate</button>
          <button role="radio" aria-checked={s.sound === "off"} className={s.sound === "off" ? "on" : ""} onClick={() => setS({ sound: "off" })} disabled={on}>Silent</button>
        </div>
      </div>
    </div>
  );
}

/** Settings of an Audio clip node: its file and the part of it a video will speak. */
export function AudioClipEditor({ projectId, audioId, start, end, onChange }: { projectId: string; audioId?: string; start: string; end: string; onChange: (changes: Record<string, string>) => void }) {
  const { track, importTrack } = useVoice(projectId, audioId);
  const own = audioId && track?.id === audioId ? track : null;
  const take = (t: VoiceTrack) => onChange({ audioId: t.id, start: "0", end: String(Math.min(30, Math.round(t.durationMs / 100) / 10)) });
  return (
    <div className="voice">
      {!own && (
        <div className="voice-empty">
          <span className="muted small">Choose the audio this node holds: a voice-over, a line of dialogue or any sound a character should speak.</span>
          <button className="btn generate" onClick={async () => { const t = await importTrack(); if (t) take(t); }}><Icon name="clip" size={15} />Import audio…</button>
          {track && !audioId && <button className="btn small" onClick={() => take(track)}>Use “{track.name}”</button>}
        </div>
      )}
      {own && (
        <>
          <div className="voice-track"><Icon name="film" size={12} /><span className="grow" title={own.name}>{own.name}</span><span className="muted">{fmt(own.durationMs / 1000)}</span>
            <button className="link" onClick={async () => { const t = await importTrack(); if (t) take(t); }}>Replace</button></div>
          <ClipPicker track={own} start={Number(start || 0)} end={Number(end || 0)} active onRange={(a, b) => onChange({ start: String(a), end: String(b) })} />
          <p className="muted small">Connect this node to a video node’s “audio” input: the clip (2–30 seconds) is sent with the request and the character speaks it with matching lips.</p>
        </>
      )}
    </div>
  );
}
