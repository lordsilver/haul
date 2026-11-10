import { beforeEach, describe, expect, test } from 'vitest';
import { pickMany, pickedNow } from '@/lib/selection';

const order = ['a', 'b', 'c', 'd', 'e'];
const picked = () => [...pickedNow()].sort();

describe('pickMany', () => {
  // Nothing picked and no anchor.
  beforeEach(() => pickMany([], []));

  test('a plain pick replaces the selection', () => {
    pickMany(['b'], order);
    pickMany(['c'], order);
    expect(picked()).toEqual(['c']);
  });

  test('a toggle adds a row, and drops it the second time', () => {
    pickMany(['b'], order);
    pickMany(['d'], order, { toggle: true });
    expect(picked()).toEqual(['b', 'd']);
    pickMany(['b'], order, { toggle: true });
    expect(picked()).toEqual(['d']);
  });

  test('a toggle of several drops them only when all are picked', () => {
    pickMany(['a'], order);
    pickMany(['a', 'b'], order, { toggle: true });
    expect(picked()).toEqual(['a', 'b']);
    pickMany(['a', 'b'], order, { toggle: true });
    expect(picked()).toEqual([]);
  });

  test('a range runs from the anchor, either way, in display order', () => {
    pickMany(['b'], order);
    pickMany(['d'], order, { range: true });
    expect(picked()).toEqual(['b', 'c', 'd']);
    // The anchor stays where the range started.
    pickMany(['a'], order, { range: true });
    expect(picked()).toEqual(['a', 'b']);
  });

  test('a range replaces the selection, and adds to it with a toggle', () => {
    pickMany(['a'], order);
    pickMany(['e'], order, { toggle: true });
    pickMany(['c'], order, { range: true });
    expect(picked()).toEqual(['c', 'd', 'e']);
    pickMany(['a'], order);
    pickMany(['e'], order, { toggle: true });
    pickMany(['c'], order, { range: true, toggle: true });
    expect(picked()).toEqual(['a', 'c', 'd', 'e']);
  });

  test('a range without an anchor in view is a plain pick', () => {
    pickMany(['c'], order, { range: true });
    expect(picked()).toEqual(['c']);
    pickMany(['x'], ['x', ...order]);
    pickMany(['d'], order, { range: true });
    expect(picked()).toEqual(['d']);
  });
});
