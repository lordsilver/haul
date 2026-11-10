import { describe, expect, test } from 'vitest';
import { archive, item, stateWith } from '@/test/data';
import { entriesOf, matchesQuery } from '@/views/downloads/entries';

describe('matchesQuery', () => {
  const it = item({
    name: 'Video.MKV',
    package: 'Example.Set',
    host: 'host.example',
    mirrors: ['host.example', 'mirror.example'],
    source_name: 'Example list',
  });

  test('an empty query matches everything', () => {
    expect(matchesQuery(it, '')).toBe(true);
  });

  test('matches the name, package, host, wrapper and every mirror, whatever their case', () => {
    for (const query of ['video.mkv', 'example.set', 'host.example', 'example list', 'mirror']) {
      expect(matchesQuery(it, query), query).toBe(true);
    }
  });

  test('a query found nowhere does not match', () => {
    expect(matchesQuery(it, 'archive')).toBe(false);
    expect(matchesQuery(item({ source_name: null }), 'example list')).toBe(false);
  });
});

describe('entriesOf', () => {
  test("an archive's parts become one entry, at its first part shown, in part order", () => {
    const parts = archive({ key: 'Example', ids: ['p1', 'p2', 'p3'] });
    const a = item({ id: 'a' });
    const b = item({ id: 'b' });
    const p1 = item({ id: 'p1', archive: 'Example' });
    const p2 = item({ id: 'p2', archive: 'Example' });
    const p3 = item({ id: 'p3', archive: 'Example' });
    expect(entriesOf([a, p3, b, p1, p2], stateWith([parts]))).toEqual([
      { it: a },
      { key: 'Example', archive: parts, members: [p1, p2, p3] },
      { it: b },
    ]);
  });

  test('a part of an archive the state does not have is a row of its own', () => {
    const p1 = item({ id: 'p1', archive: 'Example' });
    expect(entriesOf([p1], stateWith([]))).toEqual([{ it: p1 }]);
  });
});
