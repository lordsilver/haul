import { report } from '@/lib/api';
import type { HostView } from '@/lib/bindings/HostView';
import { hosts as hostRoutes } from '@/lib/client';

// The preferred hosts in the order downloads try them, as the engine sorts
// the hosts.
export const preferredOrder = (hosts: HostView[]) => hosts.filter((h) => h.group === 'preferred').map((h) => h.name);

export const setPreferredOrder = (names: string[]) => hostRoutes.preferredOrder(names).catch(report);

export type Drop = { name: string; after: boolean } | null;

// Dragging a preferred host by its grip puts it elsewhere in the order. The
// drag carries a type of its own, so the window's drop zone for links and
// files leaves it alone.
export const HOST_DRAG = 'application/x-haul-host';
