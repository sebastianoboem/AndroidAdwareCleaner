-- Eseguire una volta nel progetto Supabase.
-- Nel pannello: Authentication → abilita «Anonymous sign-ins».

create table uninstall_events (
  id uuid primary key,
  owner uuid not null default auth.uid(),
  package_name text not null,
  phone_hash text not null,
  uninstalled_at timestamptz not null,
  success boolean not null,
  notes text
);

create table flag_votes (
  owner uuid not null default auth.uid(),
  package_name text not null,
  phone_hash text not null,
  flag text not null check (flag in ('system', 'trusted', 'suspicious', 'none')),
  voted_at timestamptz not null,
  primary key (owner, package_name, phone_hash)
);

alter table uninstall_events enable row level security;
alter table flag_votes enable row level security;

-- owner viene sempre da auth.uid(), mai dal client.
create function push_uninstalls(rows jsonb) returns void
  language sql security definer
  set search_path = public
as $$
  insert into uninstall_events (id, owner, package_name, phone_hash, uninstalled_at, success, notes)
  select
    (x->>'id')::uuid,
    auth.uid(),
    x->>'package_name',
    x->>'phone_hash',
    (x->>'uninstalled_at')::timestamptz,
    (x->>'success')::boolean,
    x->>'notes'
  from jsonb_array_elements(rows) x
  where auth.uid() is not null
  on conflict (id) do nothing
$$;

create function push_votes(rows jsonb) returns void
  language sql security definer
  set search_path = public
as $$
  insert into flag_votes (owner, package_name, phone_hash, flag, voted_at)
  select
    auth.uid(),
    x->>'package_name',
    x->>'phone_hash',
    x->>'flag',
    (x->>'voted_at')::timestamptz
  from jsonb_array_elements(rows) x
  where auth.uid() is not null
  on conflict (owner, package_name, phone_hash) do update
    set flag = excluded.flag, voted_at = excluded.voted_at
    where flag_votes.voted_at < excluded.voted_at
$$;

create view package_stats with (security_invoker = false) as
  select p.package_name,
    coalesce(u.n, 0) as uninstall_count,
    count(distinct v.phone_hash) filter (where v.flag = 'system')     as system_votes,
    count(distinct v.phone_hash) filter (where v.flag = 'trusted')    as trusted_votes,
    count(distinct v.phone_hash) filter (where v.flag = 'suspicious') as suspicious_votes
  from (select package_name from flag_votes union select package_name from uninstall_events) p
  left join flag_votes v using (package_name)
  left join (
    select package_name, count(*) n
    from uninstall_events
    where success
    group by 1
  ) u using (package_name)
  group by p.package_name, u.n;

revoke execute on function push_uninstalls(jsonb), push_votes(jsonb) from public, anon;
grant execute on function push_uninstalls(jsonb), push_votes(jsonb) to authenticated;
grant select on package_stats to authenticated;
