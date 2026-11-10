import { describe, expect, test } from 'vitest';
import { archiveCaption, caption } from '@/lib/captions';
import { archive, item } from '@/test/data';

describe('caption', () => {
  test('a download says how far it is', () => {
    expect(caption(item({ status: 'downloading', size: 2e9, done: 5e8 }), false)).toBe('500.0 MB of 2.00 GB · 25%');
    expect(caption(item({ status: 'downloading', done: 3000 }), false)).toBe('3 KB');
    expect(caption(item({ status: 'downloading' }), false)).toBe('Downloading');
  });

  test('a queued row says its size, or why it does not start', () => {
    expect(caption(item({ status: 'queued', size: 1e6 }), false)).toBe('Queued · 1.0 MB');
    expect(caption(item({ status: 'queued' }), false)).toBe('Queued');
    expect(caption(item({ status: 'queued', size: 1e6 }), true)).toBe('Queued · starts when you unpause');
  });

  test('a file already on disk says where comparing it stands', () => {
    expect(caption(item({ status: 'checking', verifying: 'comparing' }), false)).toBe(
      'Comparing with the file on disk…',
    );
    expect(caption(item({ status: 'resolving', verifying: 'waiting' }), false)).toBe(
      'Waiting to compare with the file on disk',
    );
    expect(caption(item({ status: 'checking', verifying: 'waiting' }), true)).toBe(
      'Waiting to compare with the file on disk (paused)',
    );
    expect(caption(item({ status: 'checking' }), false)).toBe('Checking the link…');
    expect(caption(item({ status: 'resolving' }), false)).toBe('Starting…');
  });

  test('waits and failures say when the next try is', () => {
    expect(caption(item({ status: 'waiting', wait: 90 }), false)).toBe('Retrying in 1m 30s');
    expect(caption(item({ status: 'waiting' }), false)).toBe('Waiting');
    expect(caption(item({ status: 'error', retry_in: 30 }), false)).toBe('Failed · retrying in 30s');
    expect(caption(item({ status: 'error' }), false)).toBe('Failed');
  });

  test('a paused row says whether it had started', () => {
    expect(caption(item({ status: 'paused', size: 1e6 }), false)).toBe('Not started · 1.0 MB');
    expect(caption(item({ status: 'paused' }), false)).toBe('Not started');
    expect(caption(item({ status: 'paused', size: 1e6, done: 5e5 }), false)).toBe('Paused · 500 KB of 1.0 MB');
  });

  test('the other states', () => {
    expect(caption(item({ status: 'captcha' }), false)).toBe('Needs you');
    expect(caption(item({ status: 'passing' }), false)).toBe('Haul opens its page by itself, in turn');
    expect(caption(item({ status: 'done', size: 1e6 }), false)).toBe('Completed · 1.0 MB');
    expect(caption(item({ status: 'done', existing: true }), false)).toBe('Already downloaded');
  });
});

describe('archiveCaption', () => {
  test('unpacking, once asked for, comes first', () => {
    expect(archiveCaption(archive({ extract: 'waiting' }))).toBe('Waiting to extract');
    expect(archiveCaption(archive({ extract: 'extracting', extract_progress: 0.426 }))).toBe('Extracting · 42%');
    expect(archiveCaption(archive({ extract: 'done', missing: ['archive.part3.rar'] }))).toBe('Extracted');
    expect(archiveCaption(archive({ extract: 'failed', extract_error: 'bad data' }))).toBe(
      'Extraction failed: bad data',
    );
    expect(archiveCaption(archive({ extract: 'password' }))).toBe('Needs a password');
  });

  test('then the parts missing, then how the parts stand', () => {
    const tally = { done: 2, downloading: 1, waiting: 0, not_started: 0, paused: 0, captcha: 0, failed: 1 };
    expect(archiveCaption(archive({ ids: ['p1', 'p2', 'p3', 'p4'], tally }))).toBe(
      '2 of 4 done · 1 downloading · 1 failed',
    );
    expect(archiveCaption(archive({ ids: ['p1', 'p2', 'p3', 'p4'], tally, missing: ['archive.part5.rar'] }))).toBe(
      '1 part missing · 2 of 4 done · 1 downloading · 1 failed',
    );
  });
});
