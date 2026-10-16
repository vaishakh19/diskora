import { describe, expect, it } from 'vitest';
import {
  applyDuplicateLimit,
  applyOfflineGrace,
  checkFeature,
  daysRemaining,
  entitlementFromVerifiedLicense,
  ENTITLEMENT_TRANSITIONS,
  featuresFor,
  freeEntitlement,
  FREE_DUPLICATE_GROUP_LIMIT,
  isProActive,
  isValidTransition,
  maskLicenseKey,
} from './entitlements';
import type { Entitlement } from '@/types/contracts';

const NOW = Date.UTC(2026, 9, 10);

describe('featuresFor', () => {
  it('always enables core analysis', () => {
    for (const state of ['free', 'trial', 'pro-active', 'pro-expired', 'invalid'] as const) {
      expect(featuresFor(state).coreAnalysis).toBe(true);
    }
  });

  it('enables paid features only for trial and active Pro', () => {
    expect(featuresFor('free').advancedDuplicates).toBe(false);
    expect(featuresFor('free').exportReports).toBe(false);
    expect(featuresFor('trial').advancedDuplicates).toBe(true);
    expect(featuresFor('pro-active').advancedDuplicates).toBe(true);
    expect(featuresFor('pro-expired').advancedDuplicates).toBe(false);
    expect(featuresFor('invalid').advancedDuplicates).toBe(false);
  });

  it('returns a fresh object each call', () => {
    expect(featuresFor('free')).not.toBe(featuresFor('free'));
  });
});

describe('freeEntitlement', () => {
  it('is the default and carries the free duplicate limit', () => {
    const entitlement = freeEntitlement(NOW);
    expect(entitlement.state).toBe('free');
    expect(entitlement.tier).toBe('free');
    expect(entitlement.source).toBe('default');
    expect(entitlement.leaseUntil).toBeNull();
    expect(entitlement.permanent).toBe(false);
    expect(entitlement.licenseId).toBeNull();
    expect(entitlement.duplicateGroupLimit).toBe(FREE_DUPLICATE_GROUP_LIMIT);
    expect(FREE_DUPLICATE_GROUP_LIMIT).toBeGreaterThan(0);
  });
});

describe('maskLicenseKey', () => {
  it('keeps only the head and tail', () => {
    expect(maskLicenseKey('DWPRO-ABCD-EFGH-IJKL')).toBe('DWPRO-**********IJKL');
    expect(maskLicenseKey('short')).toBe('*****');
  });
});

describe('daysRemaining', () => {
  it('counts whole days', () => {
    expect(daysRemaining(NOW + 3 * 86_400_000, NOW)).toBe(3);
    expect(daysRemaining(NOW - 1, NOW)).toBe(0);
    expect(daysRemaining(null)).toBeNull();
  });
});

describe('entitlementFromVerifiedLicense', () => {
  it('activates a perpetual licence', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: null, licenseId: 'x' },
      NOW,
    );
    expect(entitlement.state).toBe('pro-active');
    expect(entitlement.tier).toBe('pro');
    expect(entitlement.leaseUntil).toBeNull();
    expect(entitlement.permanent).toBe(true);
    expect(entitlement.duplicateGroupLimit).toBe(0);
  });

  it('activates a licence inside its offline lease', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: NOW + 30 * 86_400_000, licenseId: 'x' },
      NOW,
    );
    expect(entitlement.state).toBe('pro-active');
    expect(entitlement.daysRemaining).toBe(30);
  });

  it('marks a lapsed lease on a non-permanent licence as pro-expired and drops the tier', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: false, leaseUntil: NOW - 1, licenseId: 'x' },
      NOW,
    );
    expect(entitlement.state).toBe('pro-expired');
    expect(entitlement.tier).toBe('free');
    expect(entitlement.features.advancedDuplicates).toBe(false);
  });

  it('a lapsed lease on a permanent licence suspends paid access, not ownership', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: NOW - 1, licenseId: 'x' },
      NOW,
    );
    expect(entitlement.state).toBe('pro-expired');
    expect(entitlement.tier).toBe('free');
    expect(entitlement.permanent).toBe(true);
    expect(entitlement.features.coreAnalysis).toBe(true);
    expect(entitlement.features.exportReports).toBe(false);
    expect(entitlement.duplicateGroupLimit).toBe(FREE_DUPLICATE_GROUP_LIMIT);
    expect(entitlement.reason).toContain('purchase is not affected');
  });

  it('treats a trial plan as a trial', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'trial', permanent: false, leaseUntil: NOW + 14 * 86_400_000, licenseId: 'x' },
      NOW,
    );
    expect(entitlement.state).toBe('trial');
    expect(entitlement.tier).toBe('trial');
  });

  it('carries the licence id and the device limit from the verified payload', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: null, licenseId: 'lic-1', deviceLimit: 3, activeDevices: 2 },
      NOW,
    );
    expect(entitlement.licenseId).toBe('lic-1');
    expect(entitlement.deviceLimit).toBe(3);
    expect(entitlement.activeDevices).toBe(2);
  });
});

describe('applyOfflineGrace', () => {
  const active: Entitlement = entitlementFromVerifiedLicense(
    { plan: 'pro', permanent: true, leaseUntil: NOW + 10 * 86_400_000, licenseId: 'x' },
    NOW,
  );

  it('keeps Pro working before its lease or cache grace ends', () => {
    expect(applyOfflineGrace(active, NOW + 5 * 86_400_000).state).toBe('pro-active');
  });

  it('suspends paid access immediately when the lease ends, without revoking ownership', () => {
    const expired = applyOfflineGrace(active, NOW + 10 * 86_400_000);
    expect(expired.state).toBe('pro-expired');
    expect(expired.tier).toBe('free');
    expect(expired.permanent).toBe(true);
    expect(expired.features.exportReports).toBe(false);
    expect(expired.duplicateGroupLimit).toBe(FREE_DUPLICATE_GROUP_LIMIT);
    expect(expired.reason).toContain('purchase is not affected');
  });

  it('degrades a stale cached entitlement even when its lease is perpetual', () => {
    const perpetual = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: null, licenseId: 'x' },
      NOW,
    );
    expect(applyOfflineGrace(perpetual, NOW + 30 * 86_400_000).state).toBe('pro-active');
    const stale = applyOfflineGrace(perpetual, NOW + 31 * 86_400_000);
    expect(stale.state).toBe('free');
    expect(stale.permanent).toBe(true);
    expect(stale.features.coreAnalysis).toBe(true);
    expect(stale.duplicateGroupLimit).toBe(FREE_DUPLICATE_GROUP_LIMIT);
  });

  it('leaves non-Pro entitlements alone', () => {
    expect(applyOfflineGrace(freeEntitlement(NOW), NOW).state).toBe('free');
  });
});

describe('transitions', () => {
  it('allows identity transitions', () => {
    for (const state of Object.keys(ENTITLEMENT_TRANSITIONS) as Array<keyof typeof ENTITLEMENT_TRANSITIONS>) {
      expect(isValidTransition(state, state)).toBe(true);
    }
  });

  it('allows free → pro and pro → expired', () => {
    expect(isValidTransition('free', 'pro-active')).toBe(true);
    expect(isValidTransition('pro-active', 'pro-expired')).toBe(true);
    expect(isValidTransition('pro-expired', 'pro-active')).toBe(true);
  });

  it('rejects nonsensical transitions', () => {
    expect(isValidTransition('free', 'pro-expired')).toBe(false);
    expect(isValidTransition('invalid', 'trial')).toBe(false);
  });
});

describe('checkFeature', () => {
  it('allows everything on an active Pro entitlement', () => {
    const entitlement = entitlementFromVerifiedLicense(
      { plan: 'pro', permanent: true, leaseUntil: null, licenseId: 'x' },
      NOW,
    );
    for (const feature of ['coreAnalysis', 'advancedDuplicates', 'exportReports'] as const) {
      expect(checkFeature(entitlement, feature).allowed).toBe(true);
    }
  });

  it('blocks paid features on free with an upgrade reason', () => {
    const gate = checkFeature(freeEntitlement(NOW), 'exportReports');
    expect(gate.allowed).toBe(false);
    expect(gate.reason).toBe('Diskwise Pro');
  });

  it('never blocks core analysis', () => {
    expect(checkFeature(freeEntitlement(NOW), 'coreAnalysis').allowed).toBe(true);
  });
});

describe('applyDuplicateLimit', () => {
  it('passes through when under the limit', () => {
    const groups = [1, 2, 3];
    expect(applyDuplicateLimit(groups, 25)).toEqual({ groups, limited: false });
  });

  it('truncates and flags the limit', () => {
    const groups = Array.from({ length: 40 }, (_, i) => i);
    const result = applyDuplicateLimit(groups, 25);
    expect(result.groups).toHaveLength(25);
    expect(result.limited).toBe(true);
  });

  it('treats 0 as unlimited', () => {
    const groups = Array.from({ length: 40 }, (_, i) => i);
    expect(applyDuplicateLimit(groups, 0).limited).toBe(false);
  });
});

describe('isProActive', () => {
  it('is true for trial and active Pro only', () => {
    expect(isProActive('trial')).toBe(true);
    expect(isProActive('pro-active')).toBe(true);
    expect(isProActive('free')).toBe(false);
    expect(isProActive('pro-expired')).toBe(false);
    expect(isProActive('invalid')).toBe(false);
  });
});
