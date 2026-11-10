// Made-up rows for the tests: each answers a whole view type, with only the
// fields a test names set to something of note.
import type { ArchiveView } from '@/lib/bindings/ArchiveView';
import type { ItemView } from '@/lib/bindings/ItemView';
import type { State } from '@/lib/bindings/State';

export function item(over: Partial<ItemView> = {}): ItemView {
  return {
    id: 'abc123',
    name: 'video.mkv',
    package: '',
    status: 'queued',
    error: '',
    size: 0,
    done: 0,
    speed: 0,
    host: 'host.example',
    mirrors: ['host.example'],
    download_from: null,
    tentative: false,
    looking_up: false,
    wait: 0,
    retry_in: 0,
    open_urls: [],
    open_hosts: [],
    path: null,
    existing: false,
    verifying: '',
    source: null,
    also_from: [],
    extract: '',
    extract_error: '',
    extract_progress: null,
    list: 'active',
    not_started: false,
    shown_host: 'host.example',
    preferred_host: false,
    other_hosts: 0,
    error_shown: '',
    reason: '',
    again: 0,
    failed_hosts: [],
    gone: false,
    slowed: null,
    commands: [],
    archive: null,
    source_name: null,
    ...over,
  };
}

export function archive(over: Partial<ArchiveView> = {}): ArchiveView {
  return {
    key: 'Example',
    name: 'archive.rar',
    ids: [],
    missing: [],
    status: 'queued',
    complete: false,
    size: 0,
    done: 0,
    speed: 0,
    share: 0,
    tally: { done: 0, downloading: 0, waiting: 0, not_started: 0, paused: 0, captcha: 0, failed: 0 },
    hosts: [],
    other_hosts: 0,
    all_hosts: [],
    slowed: null,
    download_from: null,
    package: '',
    source: null,
    source_name: null,
    extract: '',
    extract_error: '',
    extract_progress: null,
    commands: [],
    ...over,
  };
}

// Only the archives matter to `entriesOf`; the rest of the state is never read.
export const stateWith = (archives: ArchiveView[]) => ({ archives }) as State;
