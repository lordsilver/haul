// A host's own settings: account, folder, route, and whether it is on.
import { KeyRound } from 'lucide-react';
import { type FormEvent, useState } from 'react';
import { ConfirmButton } from '@/components/confirm-button';
import { Infobar } from '@/components/infobar';
import { SavingSwitch } from '@/components/saving-switch';
import { AccountField, SettingCard, SettingRow } from '@/components/setting-card';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { report } from '@/lib/api';
import type { AccountForm } from '@/lib/bindings/AccountForm';
import type { HostView } from '@/lib/bindings/HostView';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { hosts as hostRoutes } from '@/lib/client';
import { envNote } from '@/lib/settings';
import { toast } from '@/lib/toast';

export const setRoute = (name: string, route: 'proxy' | 'direct') => hostRoutes.route(name, route);

// The subfolder for one host's files.
export function FolderForm({ h, dir }: { h: HostView; dir: string }) {
  const [folder, setFolder] = useState(h.folder);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const chosen = folder.trim();
    try {
      await hostRoutes.folder(h.name, chosen);
      toast(
        chosen ? `Files from ${h.name} go to ${chosen}` : `Files from ${h.name} go to the download folder`,
        'success',
      );
    } catch (error) {
      report(error);
    }
  };
  return (
    <form className="flex flex-col gap-1.5" onSubmit={submit}>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={folder}
          placeholder="Subfolder"
          aria-label={`Folder for files from ${h.name}`}
          className="max-w-xs"
          onChange={(e) => setFolder(e.target.value)}
        />
        <Button type="submit" variant="outline" disabled={folder.trim() === h.folder}>
          Save
        </Button>
      </div>
      <p className="text-xs text-muted-foreground">
        Files from {h.name} go to this folder inside {dir}. Leave it empty for the download folder itself.
      </p>
    </form>
  );
}

// A host's account, typed in its card: each field saved as it is left.
export function AccountPanel({ form, d }: { form: AccountForm; d: SettingsView }) {
  const env = envNote(d, ...form.fields.map((f) => f.key));
  return (
    <div className="flex flex-col gap-2">
      {env || form.note ? <p className="text-xs text-muted-foreground">{env ?? form.note}</p> : null}
      <div className="flex flex-wrap items-center gap-2">
        {form.fields.map((f) => (
          <AccountField key={f.key} d={d} name={f.key} label={f.label} secret={f.secret} />
        ))}
      </div>
    </div>
  );
}

// Accounts for hosts the list does not show yet (no links of theirs so
// far): those set, and a menu to add another.
export function OtherAccounts({ s, d }: { s: State; d: SettingsView | null }) {
  const [adding, setAdding] = useState<string[]>([]);
  const extras = s.accounts.filter((a) => !s.hosts.some((h) => h.name === a.host));
  if (!d || !extras.length) return null;
  const shown = extras.filter((a) => a.set || adding.includes(a.host));
  const more = extras.filter((a) => !shown.includes(a));
  return (
    <>
      <div className="mt-3 flex flex-wrap items-baseline gap-x-2">
        <strong className="text-sm">Accounts</strong>
        <span className="text-sm text-muted-foreground">For hosts with no links here yet.</span>
      </div>
      {shown.map((a) => (
        <SettingCard key={a.host} icon={KeyRound} title={a.host}>
          <AccountPanel form={a} d={d} />
        </SettingCard>
      ))}
      {more.length ? (
        <div>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline">
                <KeyRound />
                Add an account
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">
              {more.map((a) => (
                <DropdownMenuItem key={a.host} onSelect={() => setAdding([...adding, a.host])}>
                  {a.host}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      ) : null}
    </>
  );
}

export function RouteBar({ h, proxy }: { h: HostView; proxy: boolean }) {
  if (h.blocked && h.route === 'proxy') {
    return (
      <Infobar
        kind="critical"
        title="Looks blocked through the proxy."
        actions={
          <ConfirmButton
            variant="outline"
            size="sm"
            label="Allow direct"
            confirm="Use my real IP address?"
            action={() => setRoute(h.name, 'direct')}
          />
        }
      >
        The host answered 403/451 through it.
      </Infobar>
    );
  }
  if (proxy && h.route === 'direct') {
    return (
      <Infobar
        kind="caution"
        title="Direct."
        actions={
          <Button size="sm" variant="outline" onClick={() => setRoute(h.name, 'proxy').catch(report)}>
            Back to the proxy
          </Button>
        }
      >
        This host sees your real IP address.
      </Infobar>
    );
  }
  return null;
}

// Whether Haul uses this site.
export function HostSwitch({ h }: { h: HostView }) {
  const off = h.disabled;
  return (
    <span
      className="ml-1 flex items-center gap-2 text-sm text-muted-foreground"
      title={
        h.disabled
          ? 'Off: its links are never tried. Click to use it again.'
          : h.needs_account
            ? 'On, used only when no other host has the file: free, it needs you for each one. Set its account under the wrench to use it like any other.'
            : 'On: click to never use this host'
      }
    >
      {off ? 'Off' : 'On'}
      <Switch
        checked={!off}
        aria-label={off ? `Use ${h.name}` : `Never use ${h.name}`}
        onCheckedChange={(on) => hostRoutes.disabled(h.name, !on).catch(report)}
      />
    </span>
  );
}

// A site's own settings, opened from its card: for a host, its account and
// its folder; for any site, the way it is reached when there is a proxy.
export function HostSettings({
  h,
  s,
  d,
  account,
}: {
  h: HostView;
  s: State;
  d: SettingsView | null;
  account: AccountForm | undefined;
}) {
  const fileHost = h.kind === 'host';
  return (
    <>
      {fileHost && account && d ? (
        <SettingRow title="Account" description={account.set ? 'Set.' : 'Optional: it may lift the free limits.'}>
          <AccountPanel form={account} d={d} />
        </SettingRow>
      ) : null}
      {h.accountless !== null ? (
        <SettingRow
          title="Use like any other host without its account"
          description="Without its account each file costs a step on its page, so it is used only when no other host has the file, unless this is on."
          controls={
            <SavingSwitch
              on={h.accountless}
              label="Use like any other host without its account"
              change={(on) =>
                hostRoutes.accountless(h.name, on).then(
                  () => true,
                  (e) => {
                    report(e);
                    return false;
                  },
                )
              }
            />
          }
        />
      ) : null}
      {fileHost ? (
        <SettingRow title="Folder">
          <FolderForm h={h} dir={s.download_dir} />
        </SettingRow>
      ) : null}
      {s.proxy ? (
        <SettingRow
          title="Route"
          description={
            h.route === 'direct'
              ? 'Direct: this site sees your real IP address.'
              : 'Through the proxies, as every site goes unless you allow it direct.'
          }
          controls={
            h.route === 'direct' ? (
              <Button variant="outline" onClick={() => setRoute(h.name, 'proxy').catch(report)}>
                Back to the proxy
              </Button>
            ) : (
              <ConfirmButton
                variant="outline"
                label="Allow direct"
                confirm="Use my real IP address?"
                action={() => setRoute(h.name, 'direct')}
              />
            )
          }
        />
      ) : null}
    </>
  );
}
