import { afterEach, describe, expect, it, vi } from 'vitest';
import { __setTransport, getTransport, isPackagedOrigin } from './transport';

afterEach(() => {
  __setTransport(null);
  vi.unstubAllGlobals();
});

describe('native transport selection', () => {
  it.each([
    ['tauri:', 'localhost'],
    ['http:', 'tauri.localhost'],
    ['https:', 'tauri.localhost'],
    ['file:', ''],
  ])('recognizes packaged origin %s//%s', (protocol, hostname) => {
    expect(isPackagedOrigin({ protocol, hostname })).toBe(true);
  });

  it('keeps ordinary browser previews synthetic', () => {
    expect(isPackagedOrigin({ protocol: 'https:', hostname: 'preview.example' })).toBe(false);
    expect(isPackagedOrigin({ protocol: 'http:', hostname: 'localhost' })).toBe(false);
  });

  it('refuses to fall back to synthetic data when the native bridge is missing', async () => {
    vi.stubGlobal('window', { location: { protocol: 'http:', hostname: 'tauri.localhost' } });
    await expect(getTransport()).rejects.toThrow('native backend');
  });
});
