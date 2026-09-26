import { describe, it, expect, beforeEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { detectMessage } from '../src/media.js';

function loadFixture(name) {
  const html = readFileSync(resolve(`src-tauri/webview-inject/test/fixtures/${name}`), 'utf-8');
  document.body.innerHTML = html;
  return document.querySelector('.message');
}

const CONFIG = {
  messageSelectors: ['.message[data-mid]', '.Message[data-mid]', '[data-mid][data-peer-id]'],
  storySelectors: ['[data-story-viewer]', '.StoryViewer'],
  visibleMinWidth: 100,
  protectedAncestorDepth: 5,
};

describe('media.detectMessage', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  it('detects photo message', () => {
    const el = loadFixture('message-photo.html');
    const r = detectMessage(el, CONFIG);
    expect(r.chatId).toBe('-1001234567890');
    expect(r.messageId).toBe(123);
    expect(r.protected).toBe(false);
    expect(r.media).toHaveLength(1);
    expect(r.media[0].type).toBe('photo');
  });

  it('detects video message', () => {
    const el = loadFixture('message-video.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('video');
  });

  it('detects group with multiple media (2 photo + 1 document)', () => {
    const el = loadFixture('message-group.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media).toHaveLength(3);
    expect(r.media.map(m => m.type)).toEqual(['photo', 'photo', 'document']);
  });

  it('detects voice note', () => {
    const el = loadFixture('message-voice.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('voice');
  });

  it('detects sticker', () => {
    const el = loadFixture('message-sticker.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('sticker');
  });

  it('detects animation (video[loop][muted][autoplay])', () => {
    const el = loadFixture('message-animation.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('animation');
  });

  it('detects document via data-media-type', () => {
    const el = loadFixture('message-document.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('document');
  });

  it('marks protected message', () => {
    const el = loadFixture('message-protected.html');
    const r = detectMessage(el, CONFIG);
    expect(r.protected).toBe(true);
  });

  it('returns null for element without data-mid', () => {
    document.body.innerHTML = '<div class="message"><img src="x" /></div>';
    const el = document.querySelector('.message');
    expect(detectMessage(el, CONFIG)).toBe(null);
  });

  it('returns null for element with avatar img only', () => {
    document.body.innerHTML = `
      <div class="message" data-mid="131" data-peer-id="-1">
        <img class="avatar" src="https://cdn/avatar.jpg" />
      </div>`;
    const el = document.querySelector('.message');
    const r = detectMessage(el, CONFIG);
    expect(r).toBe(null);
  });

  it('returns null for hidden media (display:none)', () => {
    document.body.innerHTML = `
      <div class="message" data-mid="132" data-peer-id="-1">
        <img src="x" style="display:none" />
      </div>`;
    const el = document.querySelector('.message');
    expect(detectMessage(el, CONFIG)).toBe(null);
  });
});
