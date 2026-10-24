import {
  ArrowDownToLine,
  ArrowUpCircle,
  CircleCheck,
  Clipboard,
  Clock,
  FileArchive,
  FileText,
  FolderOpen,
  Gauge,
  Globe,
  Info,
  KeyRound,
  type LucideIcon,
  Monitor,
  Moon,
  Palette,
  RotateCw,
  Sun,
  Trash2,
  Wrench,
} from 'lucide-react';
import { type FormEvent, useEffect, useState } from 'react';
import { Infobar } from '@/components/infobar';
import {
  AccountField,
  LinesField,
  NumberField,
  Section,
  SettingCard,
  SettingSwitch,
  TextField,
} from '@/components/setting-card';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';
import { api, openPlace, report, save } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { go } from '@/lib/route';
import { envNote, useSettings } from '@/lib/settings';
import { THEMES, setTheme, useTheme } from '@/lib/theme';
import { toast } from '@/lib/toast';

// The account a host's card asked for: in view and ready to type once the
// page shows.
let accountFor: string | null = null;

export function showAccount(host: string) {
  accountFor = host;
  go('settings');
}

const THEME_ICONS: Record<string, LucideIcon> = { system: Monitor, light: Sun, dark: Moon };

// The desktop app picks the folder in a dialog; a browser cannot show this
// machine's folders, so the path is typed instead.
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

// The desktop app asks where to save the report; a browser downloads it.
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
    if (r.saved) toast(`Saved the report to ${r.saved}`, 'success');
  } catch (error) {
    report(error);
  }
}

function Page({ s, d }: { s: State; d: SettingsView }) {
  const theme = useTheme();
  useEffect(() => {
    if (!accountFor) return;
    const card = document.querySelector(`[data-account="${CSS.escape(accountFor)}"]`);
    accountFor = null;
    card?.scrollIntoView({ block: 'center' });
    card?.querySelector('input')?.focus({ preventScroll: true });
  }, []);
  const note = (...keys: string[]) => envNote(d, ...keys);
  return (
    <div className="flex flex-col gap-2.5">
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
      <Section>Appearance</Section>
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
            {THEMES.map(({ value, label }) => {
              const Icon = THEME_ICONS[value] ?? Monitor;
              return (
                <ToggleGroupItem key={value} value={value} className="px-3">
                  <Icon />
                  {label}
                </ToggleGroupItem>
              );
            })}
          </ToggleGroup>
        }
      />
      <Section>Downloads</Section>
      <SettingCard
        icon={ArrowDownToLine}
        title="Parallel downloads"
        description="Files downloaded at the same time."
        controls={<NumberField name="max_total" value={d.max_total} min={1} max={64} label="Parallel downloads" />}
      />
      <SettingCard
        icon={FileText}
        title="Size downloading at once"
        description="The most gigabytes of files downloading at the same time, by their full size: 20 lets four 5 GB files run, or ten of 2 GB. A bigger file still downloads on its own. 0 means no limit."
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
      <SettingCard
        icon={Gauge}
        title="Speed limit"
        description={note('rate_limit_mbps') ?? 'Shared by every transfer. 0 means no limit.'}
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
      <FolderCard s={s} d={d} />
      <SettingCard
        icon={Trash2}
        title={`Move removed files to the ${s.bin}`}
        description="When you remove a finished download from the list, its file goes too. Clear completed only tidies the list."
        controls={
          <SettingSwitch
            name="recycle_on_remove"
            on={d.recycle_on_remove}
            label={`Move removed files to the ${s.bin}`}
          />
        }
      />
      <SettingCard
        icon={ArrowDownToLine}
        title="Add dropped links and files right away"
        description="Off: links and .dlc / .txt files you drop on the window, or open with Haul, fill the Add dialog first, so you can check them before anything downloads."
        controls={
          <SettingSwitch name="skip_add_dialog" on={d.skip_add_dialog} label="Add dropped links and files right away" />
        }
      />
      <SettingCard
        icon={CircleCheck}
        title="Compare checksums of files already there"
        description="A file in the download folder with the same name and size counts as downloaded. With this on, Haul also reads it in full and compares it with the checksum its host publishes, one file at a time and never while downloads are paused: thorough, but slow on big files and spinning disks."
        controls={
          <SettingSwitch
            name="verify_existing"
            on={d.verify_existing}
            label="Compare checksums of files already there"
          />
        }
      />
      <SettingCard
        icon={FileArchive}
        title="Extract archives when their parts are done"
        description="An archive in parts (name.part1.rar, name.7z.001 and so on) is unpacked beside its parts once the last one is downloaded, one archive at a time: RAR, 7‑Zip and zip. Off: extract one with its button in the list."
        controls={
          <SettingSwitch name="auto_extract" on={d.auto_extract} label="Extract archives when their parts are done" />
        }
      />
      <SettingCard
        icon={KeyRound}
        title="Archive passwords"
        description="Tried in order on an encrypted archive, after the one typed for it. One per line."
      >
        <LinesField name="archive_passwords" lines={d.archive_passwords} label="Archive passwords, one per line" />
      </SettingCard>
      <SettingCard
        icon={Trash2}
        title={`Move extracted parts to the ${s.bin}`}
        description="Once an archive is unpacked without error, its parts go. Nothing goes when unpacking fails."
        controls={
          <SettingSwitch
            name="delete_after_extract"
            on={d.delete_after_extract}
            label={`Move extracted parts to the ${s.bin}`}
          />
        }
      />
      {s.desktop ? (
        <SettingCard
          icon={Clipboard}
          title="Watch the clipboard"
          description="Links you copy anywhere are added, when they point at a supported host, a file, a paste or a release's page on a release site."
          controls={<SettingSwitch name="watch_clipboard" on={d.watch_clipboard} label="Watch the clipboard" />}
        />
      ) : null}
      <SettingCard
        icon={ArrowUpCircle}
        title="Look for updates"
        description={
          s.desktop
            ? 'Haul asks GitHub for a newer version after it starts and every six hours, and offers to install it in the status bar.'
            : 'Haul asks GitHub for a newer version after it starts and every six hours, and says so in the status bar.'
        }
        controls={<SettingSwitch name="check_updates" on={d.check_updates} label="Look for updates" />}
      />
      <Section>When something fails</Section>
      <SettingCard
        icon={RotateCw}
        title="Retries after network trouble"
        description="Per mirror, after a dropped connection or an unreachable host; then the next mirror takes over."
        controls={
          <NumberField name="resume_attempts" value={d.resume_attempts} min={0} max={20} label="Retries" unit="times" />
        }
      />
      <SettingCard
        icon={Gauge}
        title="Give up on slow downloads"
        description={`Below this speed for ${d.min_speed_seconds === 60 ? 'a minute' : `${d.min_speed_seconds} seconds`}, the next mirror takes over, or the download fails until it is tried again. 0 turns it off; so does a speed limit.`}
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
      <SettingCard
        icon={Clock}
        title="Retry failed downloads"
        description="Tries a failed download again after this many minutes, up to three times. 0 turns it off."
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
      <Section>Accounts</Section>
      <SettingCard
        icon={KeyRound}
        title="1fichier"
        description={note('1fichier_key') ?? 'API key: no free-mode wait or captcha.'}
        controls={<AccountField d={d} name="1fichier_key" label="API key" />}
      />
      <SettingCard
        icon={KeyRound}
        title="pixeldrain"
        description={note('pixeldrain_key') ?? 'API key.'}
        controls={<AccountField d={d} name="pixeldrain_key" label="API key" />}
      />
      <SettingCard
        data-account="gofile.io"
        icon={KeyRound}
        title="gofile"
        description={
          note('gofile_token') ??
          "A premium account's token. Without one gofile gives no file to Haul, so its links are left out."
        }
        controls={<AccountField d={d} name="gofile_token" label="Account token" />}
      />
      <SettingCard
        data-account="rapidgator.net"
        icon={KeyRound}
        title="rapidgator"
        description={
          note('rapidgator_user', 'rapidgator_pass') ??
          'Premium account. Without it rapidgator gives no file to Haul, so its links are left out.'
        }
        controls={
          <>
            <AccountField d={d} name="rapidgator_user" label="Username" secret={false} />
            <AccountField d={d} name="rapidgator_pass" label="Password" />
          </>
        }
      />
      <SettingCard
        data-account="nitroflare.com"
        icon={KeyRound}
        title="nitroflare"
        description={
          note('nitroflare_user', 'nitroflare_key') ??
          'Premium account. Without it nitroflare gives no file to Haul, so its links are left out.'
        }
        controls={
          <>
            <AccountField d={d} name="nitroflare_user" label="User" secret={false} />
            <AccountField d={d} name="nitroflare_key" label="Premium key" />
          </>
        }
      />
      <Section>Advanced</Section>
      <SettingCard icon={Globe} title="User agent" description="Sent to hosts. Leave empty for Haul's default.">
        <TextField
          d={d}
          name="user_agent"
          value={d.user_agent === d.default_user_agent ? '' : d.user_agent}
          label="User agent"
          placeholder={d.default_user_agent}
          wide
        />
      </SettingCard>
      <Section>Files</Section>
      {d.config_file ? (
        <SettingCard
          icon={FileText}
          title="Settings file"
          description={
            <>
              <span className="block font-mono text-xs">{d.config_file}</span>
              Every setting on this page is stored here. Edits saved in a text editor apply within a second.
            </>
          }
          controls={
            <Button variant="outline" onClick={() => openPlace('config')}>
              Open
            </Button>
          }
        />
      ) : null}
      <SettingCard
        icon={FolderOpen}
        title="Data folder"
        description={<span className="font-mono text-xs">{s.data_dir}</span>}
        controls={
          <Button variant="outline" onClick={() => openPlace('data')}>
            Open folder
          </Button>
        }
      />
      <Section>About</Section>
      <SettingCard
        icon={Info}
        title={`Haul ${s.version}`}
        description={
          s.desktop ? 'Download manager for file hosts, desktop app.' : 'Download manager for file hosts, web UI.'
        }
      />
      <SettingCard
        icon={Wrench}
        title="Diagnostics"
        description="A report of what Haul is doing, to send along when something goes wrong: every download and wrapper with its links, state, error and last events, the hosts, the settings and the recent log. Account keys, proxy passwords and mega.nz keys are left out."
        controls={
          <Button variant="outline" onClick={() => saveDiagnostics(s.desktop)}>
            Save diagnostics…
          </Button>
        }
      />
      <SettingCard
        icon={FileText}
        title="Detailed log"
        description="Writes every request, retry and mirror choice to the log (haul.log in the data folder), for reproducing a problem before you save the report. Off, the log keeps notable events. Each download keeps its last events either way."
        controls={
          <>
            <Button variant="outline" onClick={() => openPlace('log')}>
              Show log
            </Button>
            <SettingSwitch name="detailed_log" on={d.detailed_log} label="Detailed log" />
          </>
        }
      />
    </div>
  );
}

export function Settings({ s }: { s: State }) {
  const d = useSettings(s.config_version);
  return (
    <>
      <header>
        <h1 className="text-2xl font-semibold tracking-tight">Settings</h1>
      </header>
      <div className="-mx-2 min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-2 pb-4">
        <div className="max-w-4xl">{d ? <Page s={s} d={d} /> : null}</div>
      </div>
    </>
  );
}
