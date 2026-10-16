// POST /functions/v1/dodo-webhook
//
// Signed Dodo webhook intake. LIVE CHECKOUT AND ENTITLEMENT ISSUANCE ARE DISABLED.
//
// A legacy version generated an opaque key, stored only its hash, never delivered it and did not
// sign the payload the desktop app accepts. Until fulfilment and provider reconciliation exist, we
// cannot take live payments or acknowledge live successes. Test-mode events can be recorded, and
// revocations for any already-existing licences must still be attempted.
//
// Nothing from the browser is proof of payment. Verify the Standard Webhooks signature over the
// raw body, then claim the id/body atomically. Only respond 2xx after processing *and* recording a
// completed claim. A failure, in-flight claim, or changed body must not be acknowledged as done.

import { createClient } from 'jsr:@supabase/supabase-js@2';
import { fail, json, log, preflight, requestId } from '../_shared/cors.ts';
import {
  GRANTING_EVENTS,
  REVOKING_EVENTS,
  bodyHash,
  parsePayment,
  verifyWebhook,
  webhookHeaders,
  type DodoEventType,
  type DodoWebhookPayload,
} from '../_shared/dodo.ts';

/** The environment a payment was taken in, from the metadata the checkout function wrote. */
function environmentOf(payload: DodoWebhookPayload): 'test' | 'live' | 'unknown' {
  const raw = payload.data?.metadata;
  if (raw && typeof raw === 'object') {
    const value = (raw as Record<string, unknown>).environment;
    if (value === 'live' || value === 'test') return value;
  }
  return 'unknown';
}

Deno.serve(async (request) => {
  const origin = request.headers.get('origin');
  const id = requestId();

  if (request.method === 'OPTIONS') return preflight(request) ?? new Response(null, { status: 204 });
  if (request.method !== 'POST') return fail(origin, 405, 'Use POST.', id);

  const supabaseUrl = Deno.env.get('SUPABASE_URL');
  const serviceKey = Deno.env.get('SUPABASE_SERVICE_ROLE_KEY');
  const webhookSecret = Deno.env.get('DODO_WEBHOOK_SECRET');

  if (!supabaseUrl || !serviceKey || !webhookSecret) {
    log('webhook.misconfigured', { requestId: id });
    return fail(origin, 503, 'Unavailable.', id);
  }

  // Read the body exactly once, as text. Re-serialising parsed JSON would change the signed bytes.
  const rawBody = await request.text();

  const verification = await verifyWebhook(webhookHeaders(request), rawBody, webhookSecret);
  if (!verification.ok) {
    // Deliberately terse: the reason is logged, never returned. A caller probing the endpoint learns
    // nothing about which check failed.
    log('webhook.rejected', { requestId: id, reason: verification.reason });
    return fail(origin, 401, 'Unauthorized.', id);
  }

  let payload: DodoWebhookPayload;
  try {
    payload = JSON.parse(rawBody) as DodoWebhookPayload;
  } catch {
    return fail(origin, 400, 'Bad request.', id);
  }

  const webhookId = request.headers.get('webhook-id') ?? '';
  const eventType = typeof payload.type === 'string' ? payload.type : 'unknown';

  // An earlier deployment may already have taken a live payment. Do not claim/acknowledge it or
  // mint an unusable, undeliverable key: leave the delivery retriable and alert operations. This
  // guard must be removed only together with signed-key delivery, provider payment validation and
  // an end-to-end live fulfilment test. Test-mode events and revocations continue to be handled.
  if (eventType === 'payment.succeeded' &&
      ((Deno.env.get('DODO_ENVIRONMENT') ?? 'test') !== 'test' || environmentOf(payload) !== 'test')) {
    log('webhook.live-fulfilment-disabled', { requestId: id, webhookId });
    return fail(origin, 503, 'Unavailable.', id);
  }

  const hash = await bodyHash(rawBody);

  const supabase = createClient(supabaseUrl, serviceKey, {
    auth: { persistSession: false, autoRefreshToken: false },
  });

  const claimToken = crypto.randomUUID();
  const { data: claim, error: claimError } = await supabase.rpc('claim_webhook_v2', {
    id: webhookId,
    event_type: eventType,
    body_hash: hash,
    now_ms: Date.now(),
    claim_token: claimToken,
  });
  if (claimError) {
    log('webhook.claim-failed', { requestId: id, webhookId });
    return fail(origin, 503, 'Unavailable.', id);
  }
  if (claim === 'mismatch') {
    log('webhook.id-body-mismatch', { requestId: id, webhookId });
    return fail(origin, 409, 'Conflict.', id);
  }
  if (claim === 'in-flight') return fail(origin, 503, 'Unavailable.', id);
  if (claim === 'processed') {
    return json({ received: true, duplicate: true }, { origin, status: 200 });
  }
  if (claim !== 'claimed') return fail(origin, 503, 'Unavailable.', id);

  try {
    await processEvent(supabase, webhookId, eventType as DodoEventType, payload, id);
    const { data: finished, error: finishError } = await supabase.rpc('finish_webhook_v2', {
      id: webhookId,
      claim_token: claimToken,
      error: null,
      now_ms: Date.now(),
    });
    if (finishError || finished !== true) throw new Error('webhook-finish-failed');
    return json({ received: true }, { origin, status: 200 });
  } catch (error) {
    const message = error instanceof Error ? error.message.slice(0, 200) : 'unknown';
    log('webhook.processing-failed', { requestId: id, webhookId, eventType, reason: message });
    const { error: recordError } = await supabase.rpc('finish_webhook_v2', {
      id: webhookId,
      claim_token: claimToken,
      error: message,
      now_ms: Date.now(),
    });
    if (recordError) log('webhook.failure-record-failed', { requestId: id, webhookId });
    // Never tell Dodo the event is complete if any part of the work/finish failed. The next retry
    // reclaims failed work immediately, or a crashed worker's lease after five minutes.
    return fail(origin, 503, 'Unavailable.', id);
  }
});

async function processEvent(
  supabase: ReturnType<typeof createClient>,
  webhookId: string,
  eventType: DodoEventType,
  payload: DodoWebhookPayload,
  requestId: string,
): Promise<void> {
  if ((GRANTING_EVENTS as readonly string[]).includes(eventType)) {
    await grant(supabase, payload, requestId);
  } else if ((REVOKING_EVENTS as readonly string[]).includes(eventType)) {
    await revoke(supabase, payload, eventType, webhookId, requestId);
  } else {
    // Failed, processing, or cancelled payments and pending disputes never grant entitlement.
    log('webhook.ignored', { requestId, webhookId, eventType });
  }
}

/* --------------------------------------------------------------- test grant -- */

async function grant(
  supabase: ReturnType<typeof createClient>,
  payload: DodoWebhookPayload,
  requestId: string,
): Promise<void> {
  const payment = parsePayment(payload);
  if (!payment) throw new Error('malformed-payment');

  // Live checkout is disabled until an accepted payment can produce, deliver, and later recover a
  // correctly signed licence. Merely storing the hash of an opaque, never-delivered key is *not*
  // fulfilment. Keep test-mode orders for audit, but never create a licence from them.
  if (environmentOf(payload) !== 'test' || (Deno.env.get('DODO_ENVIRONMENT') ?? 'test') !== 'test') {
    throw new Error('live-fulfilment-disabled');
  }
  const metadata = (payload.data?.metadata ?? {}) as Record<string, unknown>;
  const tierId = typeof metadata.tier_id === 'string' ? metadata.tier_id : '';
  const productId = typeof metadata.product_id === 'string' ? metadata.product_id : '';
  if (!tierId || !productId) throw new Error('missing-checkout-metadata');

  const order = await upsertOrder(supabase, payment, tierId, productId, 'succeeded', 'test');
  log('webhook.test-payment-recorded', { requestId, orderId: order.id });
}

/* -------------------------------------------------------------- revoking -- */

async function revoke(
  supabase: ReturnType<typeof createClient>,
  payload: DodoWebhookPayload,
  eventType: DodoEventType,
  webhookId: string,
  requestId: string,
): Promise<void> {
  const payment = parsePayment(payload);
  if (!payment) throw new Error('malformed-revocation');

  const { data: order, error: lookupError } = await supabase
    .from('orders')
    .select('id, status, environment')
    .eq('provider_payment_id', payment.payment_id)
    .maybeSingle<{ id: string; status: string; environment: string }>();

  // A revocation arriving before its payment, an unexpected provider shape, or a database outage
  // must NOT get a 2xx. Retrying gives the earlier event a chance to finish; persistent failures
  // remain visible for manual reconciliation instead of silently granting refunded customers.
  if (lookupError) throw new Error('revocation-order-lookup-failed');
  if (!order) throw new Error('revocation-order-not-found');

  const source = eventType.startsWith('refund') ? 'refund' : eventType.startsWith('dispute') ? 'dispute' : 'admin';

  if (source === 'refund') {
    // Do not invent a provider refund id or equate a full payment to a partial refund. Until a
    // signed Dodo refund payload is validated against its documented shape, this path stays
    // retriable rather than recording fabricated money or silently revoking on ambiguous data.
    const refundId = payload.data?.refund_id;
    const refundAmount = payload.data?.amount;
    if (typeof refundId !== 'string' || !refundId) throw new Error('refund-id-missing');
    if (typeof refundAmount !== 'number' || !Number.isSafeInteger(refundAmount) ||
        refundAmount <= 0 || refundAmount > payment.total_amount) {
      throw new Error('refund-amount-unverified');
    }
    const { error: refundError } = await supabase.from('refunds').upsert(
      {
        order_id: order.id,
        provider_refund_id: refundId,
        amount_minor_units: refundAmount,
        currency: payment.currency,
        status: 'succeeded',
        created_at: Date.now(),
      },
      { onConflict: 'provider_refund_id' },
    );
    if (refundError) throw new Error('refund-write-failed');
    const { error: orderError } = await supabase
      .from('orders').update({ status: 'refunded', updated_at: Date.now() }).eq('id', order.id);
    if (orderError) throw new Error('refund-order-update-failed');
  }

  const { data: licences, error: licencesError } = await supabase
    .from('licences')
    .select('id')
    .eq('order_id', order.id);
  if (licencesError) throw new Error('revocation-licence-lookup-failed');
  const licenceIds = (licences ?? []).map((row: { id: string }) => row.id);

  const { error: updateError } = await supabase
    .from('licences')
    .update({
      status: 'refunded',
      status_reason: source,
      status_changed_at: Date.now(),
      updated_at: Date.now(),
    })
    .eq('order_id', order.id)
    .neq('status', 'refunded');
  if (updateError) throw new Error('revocation-update-failed');

  // Retries after a partial write must not create another revocation for the same delivery.
  for (const licenceId of licenceIds) {
    const { error: revocationError } = await supabase.from('revocations').upsert({
      licence_id: licenceId,
      webhook_id: webhookId,
      reason: source,
      source,
      actor: 'dodo-webhook',
      created_at: Date.now(),
    }, { onConflict: 'licence_id,webhook_id' });
    if (revocationError) throw new Error('revocation-write-failed');
  }

  const { error: auditError } = await supabase.from('audit_log').insert({
    at_ms: Date.now(),
    actor: 'dodo-webhook',
    action: `licence.${source}`,
    order_id: order.id,
    detail: `Licence revoked after ${source}.`,
  });
  if (auditError) throw new Error('revocation-audit-failed');

  log('webhook.revoke.done', { requestId, orderId: order.id, source });
}

/* ----------------------------------------------------------------- orders -- */

async function upsertOrder(
  supabase: ReturnType<typeof createClient>,
  payment: {
    payment_id: string;
    total_amount: number;
    currency: string;
    customer: { email: string; name?: string; customer_id?: string };
    metadata?: Record<string, unknown>;
  },
  tierId: string,
  productId: string,
  status: string,
  storedEnvironment: 'test' | 'live',
): Promise<{ id: string }> {
  const now = Date.now();
  const { data: previous, error: previousError } = await supabase
    .from('orders')
    .select('id, status, environment')
    .eq('provider_payment_id', payment.payment_id)
    .maybeSingle<{ id: string; status: string; environment: string }>();
  if (previousError) throw new Error('order-lookup-failed');
  if (previous && previous.environment !== storedEnvironment) {
    throw new Error('order-environment-mismatch');
  }
  if (previous?.status === 'refunded') return { id: previous.id };

  const { data, error } = await supabase
    .from('orders')
    .upsert(
      {
        provider: 'dodo',
        provider_payment_id: payment.payment_id,
        product_id: productId,
        tier_id: tierId,
        amount_minor_units: Math.round(payment.total_amount),
        currency: payment.currency,
        status,
        customer_email: payment.customer.email,
        customer_name: payment.customer.name ?? null,
        customer_id: payment.customer.customer_id ?? null,
        metadata: payment.metadata ?? {},
        environment: storedEnvironment,
        paid_at: status === 'succeeded' ? now : null,
        created_at: now,
        updated_at: now,
      },
      { onConflict: 'provider_payment_id' },
    )
    .select('id')
    .single<{ id: string }>();

  if (error || !data) {
    throw new Error(`order-upsert-failed:${error?.code ?? 'no-row'}`);
  }
  return data;
}
