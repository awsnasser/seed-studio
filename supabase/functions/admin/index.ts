// Seed Studio · admin function. The only place accounts are created or changed.
// It runs on Supabase with the project's own service key (provided to every Edge Function
// as SUPABASE_SERVICE_ROLE_KEY — it is never in the desktop app). Every call must carry the
// signed-in caller's token, and the caller must be an active admin.
import { createClient } from "npm:@supabase/supabase-js@2";

const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const fail = (status: number, code: string, message: string) => json(status, { error: { code, message } });

Deno.serve(async (req) => {
  if (req.method !== "POST") return fail(405, "method", "POST only");
  const db = createClient(Deno.env.get("SUPABASE_URL")!, Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!, { auth: { persistSession: false, autoRefreshToken: false } });
  const token = (req.headers.get("authorization") ?? "").replace(/^Bearer\s+/i, "");
  const { data: who } = await db.auth.getUser(token);
  if (!who?.user) return fail(401, "signed_out", "Sign in again.");
  const { data: me } = await db.from("profiles").select("id, role, disabled").eq("id", who.user.id).maybeSingle();
  if (!me || me.disabled || me.role !== "admin") return fail(403, "not_admin", "Only an admin can do this.");

  let body: Record<string, unknown>;
  try { body = await req.json(); } catch { return fail(400, "bad_request", "The request is not JSON."); }
  const text = (k: string) => (typeof body[k] === "string" ? (body[k] as string).trim() : "");
  const userId = text("userId");
  const profile = async (id: string) => (await db.from("profiles").select("*").eq("id", id).maybeSingle()).data;
  const needUser = async () => (userId ? await profile(userId) : null);

  switch (text("action")) {
    case "create_user": {
      const [email, name, password, role] = [text("email").toLowerCase(), text("name").slice(0, 60), typeof body.password === "string" ? body.password : "", text("role") || "user"];
      if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) return fail(400, "bad_email", "That email address does not look right.");
      if (password.length < 10) return fail(400, "weak_password", "The temporary password needs at least 10 characters.");
      if (role !== "admin" && role !== "user") return fail(400, "bad_role", "Role must be admin or user.");
      const { data, error } = await db.auth.admin.createUser({ email, password, email_confirm: true, user_metadata: { name } });
      if (error || !data.user) return fail(400, "create_failed", error?.message ?? "The account could not be created.");
      // The profile row is made by a database trigger; name and role are set here.
      const { error: e2 } = await db.from("profiles").update({ name, role }).eq("id", data.user.id);
      if (e2) return fail(500, "profile_failed", e2.message);
      return json(200, { user: await profile(data.user.id) });
    }
    case "set_password": {
      const password = typeof body.password === "string" ? body.password : "";
      if (!(await needUser())) return fail(404, "no_user", "That user does not exist.");
      if (password.length < 10) return fail(400, "weak_password", "The password needs at least 10 characters.");
      const { error } = await db.auth.admin.updateUserById(userId, { password });
      if (error) return fail(400, "password_failed", error.message);
      return json(200, { ok: true });
    }
    case "set_disabled": {
      const disabled = body.disabled === true;
      if (!(await needUser())) return fail(404, "no_user", "That user does not exist.");
      if (userId === me.id) return fail(400, "self", "You cannot disable your own account.");
      // A ban stops new sign-ins and token refreshes; the flag stops database access at once.
      const { error } = await db.auth.admin.updateUserById(userId, { ban_duration: disabled ? "876000h" : "none" });
      if (error) return fail(400, "disable_failed", error.message);
      await db.from("profiles").update({ disabled }).eq("id", userId);
      return json(200, { user: await profile(userId) });
    }
    case "set_role": {
      const role = text("role");
      if (!(await needUser())) return fail(404, "no_user", "That user does not exist.");
      if (role !== "admin" && role !== "user") return fail(400, "bad_role", "Role must be admin or user.");
      if (userId === me.id && role !== "admin") return fail(400, "self", "You cannot remove your own admin role.");
      await db.from("profiles").update({ role }).eq("id", userId);
      return json(200, { user: await profile(userId) });
    }
    case "rename": {
      if (!(await needUser())) return fail(404, "no_user", "That user does not exist.");
      await db.from("profiles").update({ name: text("name").slice(0, 60) }).eq("id", userId);
      return json(200, { user: await profile(userId) });
    }
    default:
      return fail(400, "unknown_action", "Unknown action.");
  }
});
