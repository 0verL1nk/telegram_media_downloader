import { describe, it, expect } from 'vitest';
import { ICONS, getIcon } from '../src/icons.js';

describe('icons', () => {
  it('contains all required icons', () => {
    const required = ['download', 'spinner', 'progress', 'check', 'retry',
                      'image', 'film', 'music', 'sticker', 'file', 'story'];
    for (const name of required) {
      expect(ICONS[name]).toBeTruthy();
    }
  });

  it('every icon is a non-empty SVG with viewBox', () => {
    for (const [name, svg] of Object.entries(ICONS)) {
      expect(svg.startsWith('<svg')).toBe(true);
      expect(svg.includes('viewBox')).toBe(true);
      expect(svg.endsWith('</svg>')).toBe(true);
      expect(svg.length).toBeGreaterThan(20);
    }
  });

  it('getIcon returns SVG by name', () => {
    expect(getIcon('download')).toBe(ICONS.download);
    expect(getIcon('unknown')).toBe('');
  });

  it('required icons present', () => {
    const required = ['download', 'spinner', 'progress', 'check', 'retry',
                      'image', 'film', 'music', 'sticker', 'file', 'story'];
    for (const name of required) {
      expect(ICONS[name]).toBeTruthy();
    }
  });
});
