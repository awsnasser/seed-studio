# Optional team setup

Seed Studio can connect to a Supabase project that you operate for accounts, administration and eight-seat sessions. Shared project editing is planned. The desktop app's local creative workflows work independently of this backend.

This is an early integration. Validate it on a test Supabase project before using it with a team. Automated tests exercise client behavior against a local mock server. They do not establish that a live deployment is configured correctly.

## Configure your Supabase project

1. Create a Supabase project and save the database password in your password manager.
2. Under **Authentication → Sign In / Providers**, enable Email and turn off **Allow new users to sign up** and **Confirm email**.
3. Under **Authentication → Users**, create your administrator account and confirm it.
4. Review and run [`supabase/001_team.sql`](../supabase/001_team.sql) in the SQL Editor.
5. Promote your account using its actual email address in the query below.

```sql
update public.profiles set role = 'admin' where email = 'you@example.com';
select email, role from public.profiles;
```

6. Deploy [`supabase/functions/admin/index.ts`](../supabase/functions/admin/index.ts) as an Edge Function named `admin`. Keep JWT verification enabled. Supabase provides the function's service credentials through its environment.
7. Copy the Project URL and publishable key from project settings.
8. In Seed Studio, open **Settings → Team**, enter those two public values, and choose **Save and check**.
9. Sign in through the person icon. Open **Shared sessions and people…** to manage accounts and sessions.

Use the admin panel to create additional accounts and give each person the public project URL and key through your team's normal setup process. Share temporary passwords through a secure channel and have users change them after signing in.

## Deployment checks

Verify that the tables created by the SQL have Row Level Security enabled. Test two ordinary users, an administrator and a disabled account. Check that session membership controls access and a ninth seat is rejected. Validate authorization before using the deployment with real team data.

The native app keeps its sign-in tokens in macOS Keychain. Provider keys stay on each Mac. The service-role key and database password belong in secret storage and must never be embedded in the desktop app or published in this repository.

## Files

| File | Purpose |
| --- | --- |
| `supabase/001_team.sql` | Profiles, workspaces, membership and access rules |
| `supabase/functions/admin/index.ts` | Administrator account-management operations |
| `src-tauri/src/cloud.rs` | Desktop client configuration and session handling |
| `src/components/Team.tsx` | Team settings and administration interface |
