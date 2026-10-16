/**
 * Licensing / entitlement.
 *
 * The entitlement object is produced by the backend and is only ever *read* here: the UI shows
 * state, but never decides it. A license check can never block scanning — the free tier is a
 * complete product on its own.
 *
 * Sales and delivery are incomplete: the price shown is the configured lifetime-tier price,
 * not a working checkout. All purchase buttons remain disabled until verified fulfilment ships.
 */

import { useState } from 'react';
import { BadgeCheck, Check, KeyRound, ShieldCheck, Sparkles, X } from 'lucide-react';
import { useApp } from '@/app/state';
import { Badge, Button, Notice, Panel, SectionTitle, TextField, cx } from '@/components/ui/primitives';
import { formatCount, formatDateTime, formatRelativeTime } from '@/lib/format';
import { entitlementHeadline, isPaid } from './entitlements';
import type { FeatureFlags } from '@/types/contracts';

const FREE_FEATURES: Array<{ label: string; key: keyof FeatureFlags }> = [
  { label: 'Unlimited manual scans of any drive or folder', key: 'coreAnalysis' },
  { label: 'Sunburst, treemap and file-list views', key: 'coreAnalysis' },
  { label: 'Large file finder with size, type and age filters', key: 'coreAnalysis' },
  { label: 'Basic duplicate detection (up to 25 groups)', key: 'coreAnalysis' },
  { label: 'Cleanup previews for folders you choose', key: 'coreAnalysis' },
  { label: 'Copy path and Reveal in File Explorer', key: 'coreAnalysis' },
];

const PRO_FEATURES: Array<{ label: string; key: keyof FeatureFlags }> = [
  { label: 'Unlimited duplicate groups, batch review and byte-for-byte verification', key: 'advancedDuplicates' },
  { label: 'Saved scan history and historical comparison', key: 'scanHistory' },
  { label: 'Advanced file filters (type + age analytics)', key: 'advancedFilters' },
  { label: 'Custom cleanup rules', key: 'customCleanupRules' },
];

export function LicensingView(): React.JSX.Element {
  const { state, actions } = useApp();
  const [key, setKey] = useState('');
  const entitlement = state.entitlement;
  const paid = isPaid(entitlement);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto" data-testid="licensing-view">
      <header className="border-b border-line bg-surface px-4 py-3">
        <h1 className="text-[13px] font-semibold text-ink">Plans and licence</h1>
        <p className="mt-0.5 text-[10px] text-ink-faint">
          Basic scanning, visualization, file inspection and cleanup previews require no payment.
          Live Pro purchases are not available yet.
        </p>
      </header>

      <div className="max-w-4xl px-4 py-3">
        {/* ----------------------------- current state ----------------------------- */}
        <Panel className="mb-3 p-3">
          <SectionTitle
            className="px-0 pt-0"
            action={
              <Badge tone={paid ? 'ok' : 'neutral'}>{entitlement.state}</Badge>
            }
          >
            Current entitlement
          </SectionTitle>
          <p className="text-[12px] text-ink">{entitlementHeadline(entitlement)}</p>
          <dl className="dw-tnum mt-2 grid grid-cols-2 gap-2 text-[11px] sm:grid-cols-4">
            <Row label="Tier" value={entitlement.tier} />
            <Row label="Source" value={entitlement.source} />
            <Row
              label="Offline lease"
              value={entitlement.leaseUntil === null ? 'not set' : formatDateTime(entitlement.leaseUntil)}
            />
            <Row
              label="Verified"
              value={entitlement.verifiedAt === null ? 'not yet' : formatRelativeTime(entitlement.verifiedAt)}
            />
          </dl>
          {entitlement.reason ? (
            <p className="mt-2 text-[10px] leading-relaxed text-ink-muted">{entitlement.reason}</p>
          ) : null}
          {entitlement.state === 'invalid' ? (
            <Notice tone="warn" className="mt-2">
              This licence could not be verified. Diskwise falls back to the free tier, which keeps
              every core feature working.
            </Notice>
          ) : null}
        </Panel>

        {/* -------------------------------- activate -------------------------------- */}
        <Panel className="mb-3 p-3">
          <SectionTitle className="px-0 pt-0">Activate a licence</SectionTitle>
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void actions.activateLicense(key.trim()).then(() => setKey(''));
            }}
          >
            <TextField
              label="Signed licence payload"
              placeholder="payload.signature"
              value={key}
              onChange={(event) => setKey(event.target.value)}
              className="h-8 w-72 font-mono text-[12px]"
            />
            <Button type="submit" variant="primary" disabled={key.trim().length === 0}>
              <KeyRound aria-hidden className="size-3.5" />
              Activate
            </Button>
            {paid ? (
              <Button type="button" variant="ghost" onClick={() => void actions.deactivateLicense()}>
                Deactivate
              </Button>
            ) : null}
          </form>
          <p className="mt-2 text-[10px] leading-relaxed text-ink-faint">
            Only a previously issued, correctly signed device payload can activate here. The live
            purchase, delivery and recovery flow is not connected. Scanning stays free and never
            requires an account.
          </p>
        </Panel>

        {/* --------------------------------- plans --------------------------------- */}
        <div className="mb-3 grid gap-3 sm:grid-cols-2">
          <PlanCard
            title="Free"
            price="₹0"
            period="forever"
            features={FREE_FEATURES}
            entitlement={entitlement}
            active={!paid}
            cta={paid ? 'Included' : 'Current plan'}
          />
          <PlanCard
            title="Diskwise Pro (planned)"
            price="₹1,499 one time"
            period="Configured price; checkout disabled"
            features={PRO_FEATURES}
            entitlement={entitlement}
            active={paid}
            highlight
            cta={paid ? 'Active' : 'Not available for purchase'}
          />
        </div>

        <Notice tone="warn" className="mb-3">
          Pro is not for sale: live checkout is disabled until payment verification, signed licence
          delivery, activation and refund recovery work end-to-end. The configured amount is not a
          verified market price. Scheduled scans and report exports are not implemented.
        </Notice>

        {/* ------------------------------- comparison ------------------------------- */}
        <Panel className="mb-3 p-3">
          <SectionTitle className="px-0 pt-0">Plan comparison</SectionTitle>
          <table className="w-full text-left text-[11px]">
            <thead>
              <tr className="border-b border-line text-[10px] uppercase tracking-wide text-ink-faint">
                <th className="py-1.5 font-semibold">Capability</th>
                <th className="py-1.5 text-center font-semibold">Free</th>
                <th className="py-1.5 text-center font-semibold">Pro</th>
              </tr>
            </thead>
            <tbody>
              {[
                { label: 'Drive and folder scans', free: 'Unlimited', pro: 'Unlimited' },
                { label: 'Sunburst / treemap / file list', free: 'Yes', pro: 'Yes' },
                { label: 'Large file finder', free: 'Basic', pro: 'Advanced filters' },
                { label: 'Duplicate groups', free: formatCount(entitlement.duplicateGroupLimit), pro: 'Unlimited' },
                { label: 'Byte-for-byte verification', free: '—', pro: 'Yes' },
                { label: 'Saved scan history', free: 'Session', pro: 'Full + comparison' },
                { label: 'Scheduled scans', free: '—', pro: 'Not implemented' },
                { label: 'Exportable reports', free: '—', pro: 'Not implemented' },
                { label: 'Custom cleanup rules', free: '—', pro: 'Yes' },
              ].map((row) => (
                <tr key={row.label} className="border-b border-line/60">
                  <td className="py-1.5 text-ink">{row.label}</td>
                  <td className="py-1.5 text-center text-ink-muted">{row.free}</td>
                  <td className="py-1.5 text-center text-ink-muted">{row.pro}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Panel>

        {/* ------------------------------ event log ------------------------------ */}
        {state.entitlementEvents.length > 0 ? (
          <Panel className="p-3">
            <SectionTitle className="px-0 pt-0">Entitlement events</SectionTitle>
            <ul className="space-y-1">
              {state.entitlementEvents.map((event) => (
                <li key={`${event.at}-${event.to}`} className="dw-tnum flex flex-wrap items-baseline gap-2 text-[10px]">
                  <span className="text-ink-faint">{formatDateTime(event.at)}</span>
                  <span className="text-ink">
                    {event.from} → {event.to}
                  </span>
                  <span className="min-w-0 flex-1 text-ink-muted">{event.reason}</span>
                </li>
              ))}
            </ul>
          </Panel>
        ) : null}

        <Panel className="mt-3 p-3">
          <SectionTitle className="px-0 pt-0">How entitlement works</SectionTitle>
          <ul className="space-y-1.5 text-[11px] leading-relaxed text-ink-muted">
            <li className="flex items-start gap-1.5">
              <ShieldCheck aria-hidden className="mt-0.5 size-3 shrink-0 text-ok" />
              Feature gating is a single service. No pricing logic is scattered through the codebase.
            </li>
            <li className="flex items-start gap-1.5">
              <ShieldCheck aria-hidden className="mt-0.5 size-3 shrink-0 text-ok" />
              A licence check never blocks scanning. If verification fails, Diskwise keeps working on
              the free tier.
            </li>
            <li className="flex items-start gap-1.5">
              <ShieldCheck aria-hidden className="mt-0.5 size-3 shrink-0 text-ok" />
              Entitlement states: free, pro trial, active pro, expired pro, invalid / revoked. Each
              one maps to an explicit feature set.
            </li>
            <li className="flex items-start gap-1.5">
              <ShieldCheck aria-hidden className="mt-0.5 size-3 shrink-0 text-ok" />
              Cached entitlements are honoured offline and expire on their own schedule.
            </li>
          </ul>
        </Panel>
      </div>
    </div>
  );
}

function PlanCard({
  title,
  price,
  period,
  features,
  entitlement,
  active,
  highlight,
  cta,
}: {
  title: string;
  price: string;
  period: string;
  features: Array<{ label: string; key: keyof FeatureFlags }>;
  entitlement: { features: FeatureFlags };
  active: boolean;
  highlight?: boolean;
  cta: string;
}): React.JSX.Element {
  return (
    <Panel className={cx('p-3', highlight && 'border-accent', active && 'bg-accent-soft')}>
      <div className="flex items-start justify-between gap-2">
        <div>
          <p className="flex items-center gap-1.5 text-[13px] font-semibold text-ink">
            {highlight ? <Sparkles aria-hidden className="size-3.5 text-accent" /> : null}
            {title}
          </p>
          <p className="dw-tnum mt-0.5 text-[15px] font-semibold text-ink">{price}</p>
          <p className="text-[10px] text-ink-faint">{period}</p>
        </div>
        {active ? (
          <Badge tone="ok">
            <BadgeCheck aria-hidden className="size-3" />
            Active
          </Badge>
        ) : null}
      </div>
      <ul className="mt-2 space-y-1">
        {features.map((feature) => (
          <li key={feature.label} className="flex items-start gap-1.5 text-[11px] leading-relaxed text-ink-muted">
            {entitlement.features[feature.key] ? (
              <Check aria-hidden className="mt-0.5 size-3 shrink-0 text-ok" />
            ) : (
              <X aria-hidden className="mt-0.5 size-3 shrink-0 text-ink-faint" />
            )}
            <span>{feature.label}</span>
          </li>
        ))}
      </ul>
      <Button
        size="sm"
        className="mt-2 w-full justify-center"
        variant={active ? 'secondary' : 'primary'}
        disabled
      >
        {cta}
      </Button>
    </Panel>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[10px] text-ink-faint">{label}</dt>
      <dd className="truncate text-[11px] text-ink" title={value}>
        {value}
      </dd>
    </div>
  );
}
