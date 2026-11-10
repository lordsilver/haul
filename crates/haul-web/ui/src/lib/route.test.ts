import { describe, expect, test } from 'vitest';
import { decoded } from '@/lib/route';

describe('decoded', () => {
  test('decodes a percent-encoded link', () => {
    expect(decoded('https%3A%2F%2Fsite.example%2Fvideo.mkv')).toBe('https://site.example/video.mkv');
  });

  test('keeps a % that starts no escape and decodes the rest', () => {
    expect(decoded('https://site.example/100%-done%20archive.part1.rar')).toBe(
      'https://site.example/100%-done archive.part1.rar',
    );
  });

  test('decodes a character of several bytes as one', () => {
    expect(decoded('caf%C3%A9 100%')).toBe('café 100%');
  });

  test('keeps escapes that are no character as they came', () => {
    expect(decoded('%C3%28 and %41%')).toBe('%C3( and A%');
  });
});
