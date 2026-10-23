import { Globe, KeyRound, Monitor, RotateCw, Shield, Wrench } from 'lucide-react';
import { useEffect, useState } from 'react';
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
import { Checkbox } from '@/components/ui/checkbox';
import { Label } from '@/components/ui/label';
import { api, report, save } from '@/lib/api';
import type { SettingsView } from '@/lib/bindings/SettingsView';
import type { State } from '@/lib/bindings/State';
import { envNote, useSettings } from '@/lib/settings';
import { toast } from '@/lib/toast';

// Asks before the line is cut, in place, with what it does to downloads; no
// timer to race.
function Redial({ s }: { s: State }) {
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const r = s.reconnect;
  if (r.busy || busy) {
    return (
      <Button variant="outline" disabled>
        <RotateCw className="animate-spin" />
        Getting a new IP…
      </Button>
    );
  }
  if (r.blocked) {
    return (
      <Button variant="outline" disabled title={r.blocked}>
        <RotateCw />
        Get a new IP
      </Button>
    );
  }
  if (!asking) {
    return (
      <Button variant="outline" onClick={() => setAsking(true)}>
        <RotateCw />
        Get a new IP
      </Button>
    );
  }
  const onLine = s.items.filter((i) => i.status === 'downloading').length;
  const what = onLine
    ? `${onLine} ${onLine === 1 ? 'download pauses' : 'downloads pause'} and then resume.`
    : 'Nothing is downloading.';
  return (
    <>
      <span className="text-sm text-muted-foreground">Cuts your line for a few seconds. {what}</span>
      <Button
        onClick={async () => {
          setAsking(false);
          setBusy(true);
          try {
            const out = await api<{ address: string }>('/api/reconnect');
            toast(`New IP: ${out.address}`, 'success');
          } catch (error) {
            report(error);
          } finally {
            setBusy(false);
          }
        }}
      >
        <RotateCw />
        Cut the line now
      </Button>
      <Button variant="ghost" onClick={() => setAsking(false)}>
        Cancel
      </Button>
    </>
  );
}

// Getting this line a new public address from the router: what was found,
// the button that does it now, the switch that lets Haul do it on its own,
// and the router's login or a command for any other router.
function YourLine({ s, d }: { s: State; d: SettingsView }) {
  useEffect(() => {
    api('/api/router').catch(() => {});
  }, []);
  const r = s.reconnect;
  const found = r.router
    ? `${r.router} found on your network.`
    : 'No router answered on this network yet; a command of your own works with any.';
  return (
    <>
      <SettingCard
        icon={Globe}
        title="Router"
        description={`${found} Hosts limit what one address may fetch; a new IP usually brings a fresh allowance.`}
        controls={<Redial s={s} />}
      >
        {/* What stands in the way, or how the last attempt went. */}
        {r.blocked ? (
          <Infobar kind="caution" title="Cannot get a new IP yet.">
            {`${r.blocked.charAt(0).toUpperCase()}${r.blocked.slice(1)}.`}
          </Infobar>
        ) : r.note ? (
          <Infobar kind={r.failed ? 'critical' : 'success'}>{r.note}</Infobar>
        ) : null}
      </SettingCard>
      <SettingCard
        icon={RotateCw}
        title="Get a new IP on its own"
        description={
          r.blocked ? (
            <span className="text-warn-strong">Off until Haul can get a new IP: see Router above.</span>
          ) : (
            'When a host has slowed your address for the day, or asked it to wait a long while, and no proxy can take over. At most every ten minutes; each redial cuts the line for a few seconds.'
          )
        }
        controls={
          <SettingSwitch
            name="reconnect"
            on={r.blocked && !d.reconnect ? false : d.reconnect}
            disabled={Boolean(r.blocked) && !d.reconnect}
            label="Get a new IP on its own"
          />
        }
      />
      <SettingCard
        icon={KeyRound}
        title="Router login (TR-064)"
        description={
          envNote(d, 'router_user', 'router_password') ??
          'For a router that redials only over TR-064 with a login, such as a FRITZ!Box (its user is under System › FRITZ!Box Users). A router that redials over UPnP needs none. Not used while a reconnect command is set.'
        }
        controls={
          <>
            <AccountField d={d} name="router_user" label="User" secret={false} />
            <AccountField d={d} name="router_password" label="Password" />
          </>
        }
      />
      <SettingCard
        icon={Wrench}
        title="Reconnect command"
        description="Instead of the router: for one that offers neither UPnP nor TR-064, or a VPN app's own command. When set, Haul runs it in the system's shell and leaves the router alone."
      >
        <TextField
          d={d}
          name="reconnect_command"
          value={d.reconnect_command}
          label="Reconnect command"
          placeholder="my-router-cli reconnect"
          wide
        />
      </SettingCard>
    </>
  );
}

// NordVPN: its SOCKS5 servers, least loaded first, from the countries picked.
function NordVpn({ s, d }: { s: State; d: SettingsView }) {
  const v = s.services;
  const listed = v.nordvpn_countries.length > 0;
  useEffect(() => {
    if (!listed) api('/api/nordvpn/servers').catch(() => {});
  }, [listed]);
  const chosen = new Set(d.nordvpn_countries);
  const check = async () => {
    try {
      await api('/api/nordvpn/servers');
      toast("Checked NordVPN's servers", 'success');
    } catch (error) {
      report(error);
    }
  };
  const fixed = envNote(d, 'nordvpn_user', 'nordvpn_password');
  return (
    <>
      <SettingCard
        icon={KeyRound}
        title="NordVPN"
        description={
          fixed ??
          "Its SOCKS5 servers join the proxies, each its own address. Use the service credentials (Nord Account › NordVPN › manual setup), not the account's login."
        }
        controls={
          <>
            <AccountField d={d} name="nordvpn_user" label="Service user" secret={false} />
            <AccountField d={d} name="nordvpn_password" label="Service password" />
            {d.accounts.nordvpn_password || fixed ? (
              <SettingSwitch name="nordvpn_on" on={d.nordvpn_on} label="Use NordVPN" />
            ) : null}
          </>
        }
      />
      <SettingCard
        icon={Shield}
        title="Servers"
        description={
          <>
            {v.error ? <span className="block text-destructive">{v.error}</span> : null}
            {v.nordvpn.length
              ? 'Each is checked through itself before use, and again every hour; one that does not answer gives way to the next least busy.'
              : 'Set the service credentials and a number of servers to use them.'}
          </>
        }
        controls={
          <>
            {v.nordvpn.length || v.error ? (
              <Button variant="outline" onClick={check}>
                <RotateCw />
                Check again
              </Button>
            ) : null}
            <NumberField
              name="nordvpn_servers"
              value={d.nordvpn_servers}
              min={0}
              max={100}
              label="Servers"
              unit="servers"
            />
          </>
        }
      >
        {/* Each server in use: where it is, its name, the address downloads
            through it come from (once checked) and how busy it is. */}
        {v.nordvpn.length ? (
          <ul className="divide-y rounded-lg border text-sm">
            {v.nordvpn.map((x) => (
              <li key={x.host} className="flex flex-wrap items-center gap-x-4 px-3 py-1.5">
                <span className="min-w-40 flex-1">
                  {x.city || x.country} {x.host.split('.')[0]?.replace('socks-', '')}
                </span>
                <span className="font-mono text-xs">{x.address ?? 'not checked yet'}</span>
                <span className="w-20 text-right text-muted-foreground">{x.load}% busy</span>
              </li>
            ))}
          </ul>
        ) : null}
      </SettingCard>
      {listed ? (
        <SettingCard
          icon={Globe}
          title="Countries"
          description="None ticked means every country; the least loaded servers come first."
        >
          <div className="flex flex-wrap gap-2">
            {v.nordvpn_countries.map(([name, n]) => (
              <Label
                key={name}
                className="flex items-center gap-2 rounded-full border px-3 py-1 font-normal has-data-[state=checked]:bg-accent"
              >
                <Checkbox
                  checked={chosen.has(name)}
                  onCheckedChange={(on) => {
                    const next = new Set(d.nordvpn_countries);
                    if (on) next.add(name);
                    else next.delete(name);
                    save({ nordvpn_countries: [...next] }, null);
                  }}
                />
                {name} ({n})
              </Label>
            ))}
          </div>
        </SettingCard>
      ) : null}
    </>
  );
}

// The integrations, grouped by what the user already has: the line's
// router, proxies of any kind, and VPN services whose servers become
// proxies.
function Page({ s, d }: { s: State; d: SettingsView }) {
  const pool = s.pool_summary;
  const proxies = Boolean(d.proxy || d.proxies.length);
  return (
    <div className="flex flex-col gap-2.5">
      <SettingCard
        icon={Shield}
        title={`In use: ${pool.label}`}
        description={
          pool.why ? (
            <span className="text-warn-strong">{pool.why}</span>
          ) : pool.configured ? (
            pool.sources.map((x) => `${x.name}: ${x.status}`).join(' · ')
          ) : (
            'No proxies or VPN set up yet: every download uses your line. Add them below.'
          )
        }
        controls={
          pool.configured ? <SettingSwitch name="use_proxy" on={d.use_proxy} label="Use proxies and VPN" /> : null
        }
      />
      <Section>Your line</Section>
      <YourLine s={s} d={d} />
      {proxies || s.services.nordvpn.length ? (
        <SettingCard
          icon={Monitor}
          title="Your line in the pool"
          description="Your own connection as one more address beside the proxies and VPN servers. Hosts then see your real IP address for the downloads it takes."
          controls={<SettingSwitch name="proxy_with_direct" on={d.proxy_with_direct} label="Your line in the pool" />}
        />
      ) : null}
      <Section>Proxies</Section>
      {proxies ? (
        <SettingCard
          icon={Shield}
          title="Use the proxies"
          description="Off keeps them listed but leaves them out; VPN servers go on."
          controls={<SettingSwitch name="proxies_on" on={d.proxies_on} label="Use the proxies" />}
        />
      ) : null}
      <SettingCard
        icon={Shield}
        title="Proxy"
        description={
          envNote(d, 'proxy') ??
          'Every host goes through the proxies unless you allow it direct under Hosts. Leave empty to connect directly.'
        }
      >
        <TextField
          d={d}
          name="proxy"
          value={d.proxy}
          label="Proxy"
          placeholder="socks5://user:pass@host:1080"
          secret
          wide
        />
      </SettingCard>
      <SettingCard
        icon={Shield}
        title="More proxies"
        description="One per line. Each download takes the proxy with the fewest downloads from its host, so a host's limits per address (downloads at once, daily quotas, waits) apply to each one. A VPN's SOCKS5 servers, local WireGuard-to-SOCKS5 ports or a bought list all fit."
      >
        <LinesField
          name="proxies"
          lines={d.proxies}
          label="More proxies, one per line"
          placeholder={'socks5://user:pass@amsterdam.example:1080\nsocks5://127.0.0.1:1081'}
        />
      </SettingCard>
      {proxies ? (
        <SettingCard
          icon={RotateCw}
          title="New connection for every download"
          description="For a proxy that gives each connection another address (a rotating endpoint), so each download gets its own."
          controls={
            <SettingSwitch
              name="proxy_fresh_connections"
              on={d.proxy_fresh_connections}
              label="New connection for every download"
            />
          }
        />
      ) : null}
      <Section>VPN services</Section>
      <NordVpn s={s} d={d} />
    </div>
  );
}

export function Connections({ s }: { s: State }) {
  const d = useSettings(s.config_version);
  return (
    <>
      <header className="flex flex-col gap-1">
        <h1 className="text-2xl font-semibold tracking-tight">Connections</h1>
        <p className="max-w-3xl text-sm text-muted-foreground">
          What downloads go out through. Hosts limit what one address may fetch (downloads at once, daily allowances,
          waits), so every address Haul can use is another share: a new one from your router, proxies, or a VPN
          service's servers.
        </p>
      </header>
      <div className="-mx-2 min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-2 pb-4">
        <div className="max-w-4xl">{d ? <Page s={s} d={d} /> : null}</div>
      </div>
    </>
  );
}
