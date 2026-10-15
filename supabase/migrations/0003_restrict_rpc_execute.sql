-- SECURITY DEFINER functions bypass table RLS. PostgreSQL grants EXECUTE to PUBLIC on new
-- functions by default; RLS alone therefore does not protect these RPC endpoints. Revoke that
-- default on both fresh and already-migrated databases, and grant only the server-side role.
-- Never expose these functions to anon/authenticated, even if a client guesses a licence UUID.
-- A vanilla Postgres CI database does not have Supabase roles; the guarded grants keep this
-- migration testable there. Production deployment MUST have service_role configured.

revoke execute on function public.active_device_count(uuid) from public;
revoke execute on function public.activate_device(uuid, text, text, text, bigint) from public;
revoke execute on function public.deactivate_device(uuid, text, text, bigint) from public;
revoke execute on function public.claim_webhook(text, text, text, bigint) from public;
revoke execute on function public.finish_webhook(text, text, bigint) from public;
revoke execute on function public.rate_limit_hit(text, text, integer, bigint, bigint) from public;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    revoke execute on function public.active_device_count(uuid),
      public.activate_device(uuid, text, text, text, bigint),
      public.deactivate_device(uuid, text, text, bigint),
      public.claim_webhook(text, text, text, bigint),
      public.finish_webhook(text, text, bigint),
      public.rate_limit_hit(text, text, integer, bigint, bigint) from anon;
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    revoke execute on function public.active_device_count(uuid),
      public.activate_device(uuid, text, text, text, bigint),
      public.deactivate_device(uuid, text, text, bigint),
      public.claim_webhook(text, text, text, bigint),
      public.finish_webhook(text, text, bigint),
      public.rate_limit_hit(text, text, integer, bigint, bigint) from authenticated;
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    grant execute on function public.active_device_count(uuid),
      public.activate_device(uuid, text, text, text, bigint),
      public.deactivate_device(uuid, text, text, bigint),
      public.claim_webhook(text, text, text, bigint),
      public.finish_webhook(text, text, bigint),
      public.rate_limit_hit(text, text, integer, bigint, bigint) to service_role;
  end if;
end;
$$;
