-- Durable, retriable webhook claims. The original claim_webhook(bool) in 0002 treated every
-- duplicate id as processed, even when its first worker failed or died before issuing a licence.
-- New callers use v2: only an actually finished delivery gets a 2xx duplicate acknowledgement.
-- Both the old and new RPCs remain service-role only; the old signatures are retained briefly for
-- a staged Edge Function deployment, but must not be called by new code.

alter table public.webhook_events
  add column processing_started_at bigint,
  add column processing_token uuid;

-- Retried work may reach the revocation write twice (for example, if the Edge isolate dies after
-- the insert but before finish_webhook_v2). Keep exactly one row per licence and delivery.
alter table public.revocations add column webhook_id text;
create unique index revocations_webhook_unique on public.revocations (licence_id, webhook_id);

-- Returns claimed | processed | in-flight | mismatch. The caller MUST return a non-2xx for
-- in-flight and mismatch, and it MUST finish a claim (or return a non-2xx) before acknowledging.
-- A 5-minute lease lets a later Dodo retry recover from an Edge isolate that died mid-request.
-- The per-attempt token prevents an older, slow worker from finishing a newer worker's claim.
create function public.claim_webhook_v2(
  id text, event_type text, body_hash text, now_ms bigint, claim_token uuid
)
returns text
language plpgsql
security definer
set search_path = public
as $$
declare
  current_row public.webhook_events%rowtype;
begin
  if id is null or length(id) not between 1 and 200
     or event_type is null or event_type !~ '^[a-z][a-z0-9_.]*$'
     or body_hash is null or body_hash !~ '^[0-9a-f]{64}$'
     or now_ms is null or now_ms <= 0 or claim_token is null then
    raise exception 'invalid_webhook_claim' using errcode = 'check_violation';
  end if;

  insert into public.webhook_events (
    webhook_id, event_type, body_hash, received_at, attempts,
    processing_started_at, processing_token
  ) values (id, event_type, body_hash, now_ms, 1, now_ms, claim_token)
  on conflict (webhook_id) do nothing;
  if found then return 'claimed'; end if;

  select * into strict current_row from public.webhook_events
    where webhook_id = id for update;
  if current_row.body_hash <> body_hash or current_row.event_type <> event_type then
    return 'mismatch';
  end if;
  if current_row.processed_at is not null then return 'processed'; end if;
  if current_row.last_error is null and current_row.processing_started_at is not null
     and current_row.processing_started_at > now_ms - 300000 then
    return 'in-flight';
  end if;

  update public.webhook_events set attempts = attempts + 1,
    last_error = null, processing_started_at = now_ms, processing_token = claim_token
    where webhook_id = id;
  return 'claimed';
end;
$$;

-- Returns false if this worker's lease was replaced, the id is unknown or it is already finished.
create function public.finish_webhook_v2(
  id text, claim_token uuid, error text, now_ms bigint
)
returns boolean
language plpgsql
security definer
set search_path = public
as $$
declare
  updated integer;
begin
  if id is null or claim_token is null or now_ms is null or now_ms <= 0 then
    raise exception 'invalid_webhook_finish' using errcode = 'check_violation';
  end if;
  update public.webhook_events
     set processed_at = case when error is null then now_ms else null end,
         last_error = case when error is null then null else left(error, 500) end,
         processing_started_at = null,
         processing_token = null
   where webhook_id = id and processing_token = claim_token and processed_at is null;
  get diagnostics updated = row_count;
  return updated = 1;
end;
$$;

revoke execute on function public.claim_webhook_v2(text, text, text, bigint, uuid) from public;
revoke execute on function public.finish_webhook_v2(text, uuid, text, bigint) from public;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    revoke execute on function public.claim_webhook_v2(text, text, text, bigint, uuid),
      public.finish_webhook_v2(text, uuid, text, bigint) from anon;
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    revoke execute on function public.claim_webhook_v2(text, text, text, bigint, uuid),
      public.finish_webhook_v2(text, uuid, text, bigint) from authenticated;
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    grant execute on function public.claim_webhook_v2(text, text, text, bigint, uuid),
      public.finish_webhook_v2(text, uuid, text, bigint) to service_role;
  end if;
end;
$$;
