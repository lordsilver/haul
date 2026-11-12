import { describe, expect, test } from 'vitest';
import { addedMessage } from '@/lib/api';

const none = { added: 0, merged: 0, duplicates: 0, invalid: 0, wrappers: 0, pages: 0, reread: 0 };

describe('addedMessage', () => {
  test('says how many rows were added', () => {
    expect(addedMessage({ ...none, added: 3 })).toBe('3 added');
    expect(addedMessage(none)).toBe('0 added');
  });

  test('leaves out "0 added" when only wrappers came', () => {
    expect(addedMessage({ ...none, wrappers: 2 })).toBe('2 wrappers to unwrap');
    expect(addedMessage({ ...none, added: 1, wrappers: 1 })).toBe('1 added · 1 wrapper to unwrap');
  });

  test('names each other outcome', () => {
    expect(addedMessage({ ...none, merged: 1, duplicates: 2, invalid: 1 })).toBe(
      '0 added · 1 merged as mirrors · 2 already in the list · 1 not http(s)',
    );
  });
});
