import {
  ArrowDownToLine,
  CircleCheck,
  FileArchive,
  FolderOpen,
  Gauge,
  Info,
  LoaderCircle,
  Palette,
  RotateCw,
  Shuffle,
  Wrench,
} from 'lucide-react';
import { type FormEvent, useState } from 'react';
import { Infobar } from '@/components/infobar';
import { PageHeader, PageScroll } from '@/components/page';
import { LinesField, NumberField, SettingCard, SettingRow, SettingSwitch } from '@/components/setting-card';
import { type OwnTab, SettingsTabs, savedTab } from '@/components/settings-tabs';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';
import { api, openPlace, report, save } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { envNote, useSettings } from '@/lib/settings';
import { THEMES, setTheme, useTheme } from '@/lib/theme';
import { toast } from '@/lib/toast';

// The desktop app picks the folder in a dialog; a browser cannot show this
// machine's folders, so the path is typed instead.
// Asks now rather than wait for the next look; found, it shows in the status bar.
function CheckNow() {
  const [busy, setBusy] = useState(false);
  const check = async () => {
    setBusy(true);
    try {
      const { version } = await api<{ version: string | null }>('/api/update/check');
      if (version) toast(`Haul ${version} is out: see the status bar`, 'success');
      else toast('You have the latest Haul');
    } catch (error) {
      report(error);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button variant="outline" disabled={busy} onClick={check}>
      {busy ? <LoaderCircle aria-hidden className="animate-spin" /> : <RotateCw aria-hidden />}
      Check now
    </Button>
  );
}

function FolderCard({ s, d }: { s: State; d: SettingsView }) {
  const [editing, setEditing] = useState(false);
  const [path, setPath] = useState(d.download_dir);
  const change = async () => {
    if (!s.desktop) {
      setPath(d.download_dir);
      setEditing(true);
      return;
    }
    try {
      const r = await api<{ cancelled?: boolean }>('/api/settings/download-dir/pick');
      if (!r.cancelled) toast('Download folder changed; unfinished downloads moved along', 'success');
    } catch (error) {
      report(error);
    }
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (await save({ download_dir: path.trim() }, 'Download folder changed')) setEditing(false);
  };
  return (
    <SettingCard
      icon={FolderOpen}
      title="Download folder"
      description={
        <>
          <span className="font-mono text-xs">{d.download_dir}</span>
          {d.download_dir_fixed ? <span className="block">Given on the command line with --dir.</span> : null}
        </>
      }
      controls={
        d.download_dir_fixed ? null : (
          <Button variant="outline" onClick={change}>
            Change…
          </Button>
        )
      }
    >
      {editing ? (
        <form className="flex flex-wrap items-center gap-2" onSubmit={submit}>
          <Input
            autoFocus
            value={path}
            aria-label="Download folder"
            className="flex-1"
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && setEditing(false)}
          />
          <Button type="submit">Save</Button>
          <Button type="button" variant="ghost" onClick={() => setEditing(false)}>
            Cancel
          </Button>
        </form>
      ) : null}
    </SettingCard>
  );
}

// Where Haul keeps its settings, queue, logs and browser data. The settings
// file is in that folder unless the command line named another one.
function DataCard({ s, d }: { s: State; d: SettingsView }) {
  const sep = s.data_dir.includes('\\') ? '\\' : '/';
  const inside = d.config_file?.startsWith(s.data_dir + sep);
  return (
    <SettingCard
      icon={FolderOpen}
      title="Data folder"
      description={
        <>
          <span className="block font-mono text-xs">{s.data_dir}</span>
          Settings, downloads list, logs and browser data.
          {d.config_file && !inside ? (
            <span className="block">
              Settings file: <span className="font-mono text-xs">{d.config_file}</span>
            </span>
          ) : null}
        </>
      }
      controls={
        <>
          {d.config_file ? (
            <Button variant="outline" onClick={() => openPlace('config')}>
              Settings file
            </Button>
          ) : null}
          <Button variant="outline" onClick={() => openPlace('data')}>
            Open
          </Button>
        </>
      }
    />
  );
}

// The desktop app saves the report to Downloads and shows it; a browser
// downloads it.
async function saveDiagnostics(desktop: boolean) {
  if (!desktop) {
    const link = document.createElement('a');
    link.href = '/api/diagnostics';
    link.download = '';
    link.click();
    return;
  }
  try {
    const r = await api<{ saved?: string }>('/api/diagnostics/save');
    if (r.saved) toast(`Saved ${r.saved}`, 'success');
  } catch (error) {
    report(error);
  }
}

function Page({ s, d, tab }: { s: State; d: SettingsView; tab: OwnTab }) {
  const theme = useTheme();
  const note = (...keys: string[]) => envNote(d, ...keys);
  const archivesCard = (
    <SettingCard icon={FileArchive} title="Archives">
      <SettingRow
        title="Extract when the last part is done"
        description="RAR, 7‑Zip and zip. Off: use the button in the list."
        controls={<SettingSwitch name="auto_extract" on={d.auto_extract} label="Extract when the last part is done" />}
      />
      <SettingRow
        title={`Then move the parts to the ${s.bin}`}
        description="Only when unpacking worked."
        controls={
          <SettingSwitch
            name="delete_after_extract"
            on={d.delete_after_extract}
            label={`Move extracted parts to the ${s.bin}`}
          />
        }
      />
      <SettingRow title="Passwords to try" description="One per line, after the one typed for the archive.">
        <LinesField name="archive_passwords" lines={d.archive_passwords} label="Archive passwords, one per line" />
      </SettingRow>
    </SettingCard>
  );
  return (
    <>
      {s.config_error ? (
        <Infobar
          kind="critical"
          title="The settings file has an error."
          actions={
            <Button size="sm" variant="outline" onClick={() => openPlace('config')}>
              Open the file
            </Button>
          }
        >
          The previous settings stay in effect until it is fixed. {s.config_error}
        </Infobar>
      ) : null}
      {tab === 'general' ? (
        <>
          <SettingCard
            icon={Palette}
            title="App theme"
            description="Follow Windows, or pick one."
            controls={
              <ToggleGroup
                type="single"
                variant="outline"
                value={theme}
                aria-label="App theme"
                onValueChange={(value) => value && setTheme(value as typeof theme)}
              >
                {THEMES.map(({ value, label, icon: Icon }) => (
                  <ToggleGroupItem key={value} value={value} className="px-3">
                    <Icon />
                    {label}
                  </ToggleGroupItem>
                ))}
              </ToggleGroup>
            }
          />
          <DataCard s={s} d={d} />
        </>
      ) : null}
      {tab === 'downloads' ? (
        <>
          <FolderCard s={s} d={d} />
          <SettingCard icon={ArrowDownToLine} title="Adding links">
            <SettingRow
              title="Add dropped links right away"
              description="Off: dropped links and files fill the Add dialog first."
              controls={
                <SettingSwitch name="skip_add_dialog" on={d.skip_add_dialog} label="Add dropped links right away" />
              }
            />
            {s.desktop ? (
              <SettingRow
                title="Watch the clipboard"
                description="Copied links to hosts, files and pastes are added."
                controls={<SettingSwitch name="watch_clipboard" on={d.watch_clipboard} label="Watch the clipboard" />}
              />
            ) : null}
          </SettingCard>
          <SettingCard icon={Gauge} title="Limits" description="Each applies on top of the others. 0 means no limit.">
            <SettingRow
              title="Speed"
              description={note('rate_limit_mbps') ?? 'Shared by every download.'}
              controls={
                <NumberField
                  name="rate_limit_mbps"
                  value={d.rate_limit_mbps}
                  min={0}
                  step={0.5}
                  label="Speed limit"
                  unit="MB/s"
                />
              }
            />
            <SettingRow
              title="Size downloading at once"
              description="By full size: 20 GB lets four 5 GB files run. A bigger file still runs alone."
              controls={
                <NumberField
                  name="max_active_gb"
                  value={d.max_active_gb}
                  min={0}
                  label="Size downloading at once"
                  unit="GB"
                />
              }
            />
            <SettingRow
              title="Parallel downloads"
              controls={
                <NumberField
                  name="max_total"
                  value={d.max_total}
                  min={1}
                  max={64}
                  label="Parallel downloads"
                  unit="files"
                />
              }
            />
          </SettingCard>
          <SettingCard icon={Shuffle} title="Slow downloads">
            <SettingRow
              title="Leave hosts that slow you down"
              description="Go on from another host, or a new IP, when a host holds a download far back."
              controls={
                <SettingSwitch name="leave_slow_hosts" on={d.leave_slow_hosts} label="Leave hosts that slow you down" />
              }
            />
            <SettingRow
              title="Give up below"
              description={`For ${d.min_speed_seconds === 60 ? 'a minute' : `${d.min_speed_seconds} seconds`}: the next host takes over, or it fails. 0 is off.`}
              controls={
                <NumberField
                  name="min_speed_kbps"
                  value={d.min_speed_kbps}
                  min={0}
                  max={1048576}
                  label="Minimum speed"
                  unit="KB/s"
                />
              }
            />
          </SettingCard>
          <SettingCard icon={RotateCw} title="Retries">
            <SettingRow
              title="After network trouble"
              description="Per host, before the next one takes over."
              controls={
                <NumberField
                  name="resume_attempts"
                  value={d.resume_attempts}
                  min={0}
                  max={20}
                  label="Retries"
                  unit="times"
                />
              }
            />
            <SettingRow
              title="Failed downloads, after"
              description="Up to three times. 0 is off."
              controls={
                <NumberField
                  name="retry_failed_minutes"
                  value={d.retry_failed_minutes}
                  min={0}
                  max={1440}
                  label="Minutes"
                  unit="min"
                />
              }
            />
          </SettingCard>
          <SettingCard icon={CircleCheck} title="Finished files">
            <SettingRow
              title="Compare checksums of files already there"
              description="A file with the same name and size counts as downloaded. On: it's also read in full and checked. Slow on big files."
              controls={
                <SettingSwitch
                  name="verify_existing"
                  on={d.verify_existing}
                  label="Compare checksums of files already there"
                />
              }
            />
            <SettingRow
              title={`Move removed files to the ${s.bin}`}
              description="When you remove a finished download from the list."
              controls={
                <SettingSwitch
                  name="recycle_on_remove"
                  on={d.recycle_on_remove}
                  label={`Move removed files to the ${s.bin}`}
                />
              }
            />
          </SettingCard>
        </>
      ) : null}
      {tab === 'archives' ? archivesCard : null}
      {tab === 'about' ? (
        <>
          <SettingCard
            icon={Info}
            title={`Haul ${s.version}`}
            description={s.desktop ? 'Desktop app.' : 'Web UI.'}
            controls={<CheckNow />}
          >
            <SettingRow
              title="Look for updates"
              description="After Haul starts, then every hour."
              controls={<SettingSwitch name="check_updates" on={d.check_updates} label="Look for updates" />}
            />
          </SettingCard>
          <SettingCard
            icon={Wrench}
            title="Report a problem"
            description="One file with your downloads, hosts, settings and log, without keys or passwords."
            controls={
              <Button variant="outline" onClick={() => saveDiagnostics(s.desktop)}>
                Save report…
              </Button>
            }
          >
            <SettingRow
              title="Live connection"
              description="Lets a diagnosing tool on this computer follow everything Haul does as it happens, with a key Haul writes to its data folder."
              controls={<SettingSwitch name="live_debug" on={d.live_debug} label="Live connection" />}
            />
          </SettingCard>
        </>
      ) : null}
    </>
  );
}

export function Settings({ s }: { s: State }) {
  const d = useSettings(s.config_version);
  const [tab, setTab] = useState<OwnTab>(savedTab);
  return (
    <>
      <PageHeader title="Settings" actions={<SettingsTabs current={tab} pick={setTab} />} />
      <PageScroll>{d ? <Page s={s} d={d} tab={tab} /> : null}</PageScroll>
    </>
  );
}
