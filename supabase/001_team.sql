-- Seed Studio · team foundation (phase 1): people, shared sessions, seats.
-- Run once in the Supabase SQL editor of the project. Safe to read: it holds no secret.
-- Rules it enforces in the database itself, whatever any app sends:
--   · accounts exist only if an admin created them (sign-up is switched off in Auth settings);
--   · a shared session holds at most 8 people;
--   · only its members (and admins) can see a session; only its owner (and admins) can change who is in it.

create table public.profiles (
  id uuid primary key references auth.users on delete cascade,
  email text not null,
  name text not null default '' check (char_length(name) <= 60),
  role text not null default 'user' check (role in ('admin', 'user')),
  disabled boolean not null default false,
  created_at timestamptz not null default now()
);

create table public.workspaces (
  id uuid primary key default gen_random_uuid(),
  name text not null check (char_length(name) between 1 and 80),
  owner uuid not null references public.profiles (id),
  created_at timestamptz not null default now()
);

create table public.workspace_members (
  workspace_id uuid not null references public.workspaces (id) on delete cascade,
  user_id uuid not null references public.profiles (id) on delete cascade,
  role text not null default 'member' check (role in ('owner', 'member')),
  added_at timestamptz not null default now(),
  primary key (workspace_id, user_id)
);
create index workspace_members_user on public.workspace_members (user_id);

-- ── helpers (security definer: they read the tables without tripping the policies below) ──
create function public.is_active() returns boolean language sql stable security definer set search_path = public as
$$ select exists (select 1 from profiles where id = auth.uid() and not disabled) $$;

create function public.is_admin() returns boolean language sql stable security definer set search_path = public as
$$ select exists (select 1 from profiles where id = auth.uid() and role = 'admin' and not disabled) $$;

create function public.is_member(ws uuid) returns boolean language sql stable security definer set search_path = public as
$$ select public.is_active() and exists (select 1 from workspace_members where workspace_id = ws and user_id = auth.uid()) $$;

create function public.is_owner(ws uuid) returns boolean language sql stable security definer set search_path = public as
$$ select public.is_active() and exists (select 1 from workspaces where id = ws and owner = auth.uid()) $$;

-- ── a profile for every account ──
create function public.handle_new_user() returns trigger language plpgsql security definer set search_path = public as $$
begin
  insert into profiles (id, email, name) values (new.id, coalesce(new.email, ''), left(coalesce(new.raw_user_meta_data ->> 'name', ''), 60));
  return new;
end $$;
create trigger on_auth_user_created after insert on auth.users for each row execute function public.handle_new_user();

-- Accounts that already exist (the first admin, created in the dashboard) get a profile too.
insert into public.profiles (id, email) select id, coalesce(email, '') from auth.users on conflict do nothing;

-- ── seats: at most 8 people in a shared session ──
create function public.check_seats() returns trigger language plpgsql security definer set search_path = public as $$
begin
  perform 1 from workspaces where id = new.workspace_id for update; -- one seat is given at a time
  if (select count(*) from workspace_members where workspace_id = new.workspace_id) >= 8 then
    raise exception 'seat_limit: a shared session holds at most 8 people' using errcode = 'P0001';
  end if;
  return new;
end $$;
create trigger workspace_seats before insert on public.workspace_members for each row execute function public.check_seats();

-- The person who creates a session is its owner and takes the first seat.
create function public.add_owner_seat() returns trigger language plpgsql security definer set search_path = public as $$
begin
  insert into workspace_members (workspace_id, user_id, role) values (new.id, new.owner, 'owner');
  return new;
end $$;
create trigger workspace_owner_seat after insert on public.workspaces for each row execute function public.add_owner_seat();

-- The owner's seat goes only when the session itself is deleted.
create function public.keep_owner_seat() returns trigger language plpgsql security definer set search_path = public as $$
begin
  if old.role = 'owner' and exists (select 1 from workspaces where id = old.workspace_id) then
    raise exception 'owner_seat: the owner cannot be removed from the session' using errcode = 'P0001';
  end if;
  return old;
end $$;
create trigger workspace_owner_stays before delete on public.workspace_members for each row execute function public.keep_owner_seat();

-- ── who may do what ──
alter table public.profiles enable row level security;
alter table public.workspaces enable row level security;
alter table public.workspace_members enable row level security;

revoke all on public.profiles, public.workspaces, public.workspace_members from anon, authenticated;
grant select on public.profiles to authenticated;
grant update (name) on public.profiles to authenticated; -- role and disabled change only through the admin function
grant select, insert, delete on public.workspaces, public.workspace_members to authenticated;
grant update (name) on public.workspaces to authenticated;

create policy profiles_read on public.profiles for select to authenticated using (public.is_active());
create policy profiles_rename_self on public.profiles for update to authenticated using (id = auth.uid() and public.is_active()) with check (id = auth.uid());

create policy workspaces_read on public.workspaces for select to authenticated using (public.is_admin() or public.is_member(id) or (owner = auth.uid() and public.is_active()));
create policy workspaces_create on public.workspaces for insert to authenticated with check (owner = auth.uid() and public.is_active());
create policy workspaces_rename on public.workspaces for update to authenticated using (public.is_admin() or public.is_owner(id)) with check (public.is_admin() or public.is_owner(id));
create policy workspaces_delete on public.workspaces for delete to authenticated using (public.is_admin() or public.is_owner(id));

create policy members_read on public.workspace_members for select to authenticated using (public.is_admin() or public.is_member(workspace_id));
create policy members_add on public.workspace_members for insert to authenticated with check (role = 'member' and (public.is_admin() or public.is_owner(workspace_id)));
create policy members_remove on public.workspace_members for delete to authenticated using (public.is_admin() or public.is_owner(workspace_id) or (user_id = auth.uid() and public.is_active()));

-- ── the first admin ──
-- After creating your own account in Authentication → Users, run this one line with your email:
--   update public.profiles set role = 'admin' where email = 'you@example.com';
