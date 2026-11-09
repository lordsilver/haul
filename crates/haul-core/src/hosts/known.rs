//! The table of sites Haul has code for: what each is, what a free user gets, the accounts it takes.

use super::*;

/// What a free user gets from a host, where it is known; an account the
/// user sets may lift it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Free {
    /// Downloads at once from one address.
    pub at_once: Option<usize>,
    /// Bytes a day one address gets at full speed.
    pub daily_bytes: Option<u64>,
    /// Bytes per second past them.
    pub slowed_to: Option<u64>,
    /// What each file asks of a free user, said as the Hosts view lists it
    /// ("a captcha per file").
    pub per_file: Option<&'static str>,
}

/// A host Haul knows: the names it goes by, the code that serves it, the
/// account settings that unlock more, what a free user gets, and whether it
/// says how much an address has left today ([`allowance`]).
#[derive(Debug)]
pub struct Known {
    pub names: &'static [&'static str],
    /// The code behind it.
    pub(super) site: &'static dyn Site,
    /// A wrapper's site, which stands for further links rather than files.
    pub(super) wraps: bool,
    pub accounts: &'static [AccountField],
    pub free: Free,
    pub tells_allowance: bool,
    /// Its page's check passes by itself in a browser from most addresses,
    /// so the desktop app opens the page on its own, out of sight, and
    /// shows it only when the check wants a person after all.
    pub page_passes: bool,
    /// Without its account it gives no decent download: a captcha for
    /// every file, or names only once a page is passed. So it is off
    /// until the user sets one or switches it on without one
    /// ([`Known::last_resort`]).
    pub needs_account: bool,
    /// What a file of it asks of a person when its account is not set.
    pub free_asks: Asks,
    /// It says what a file is (its size, whether it is still there) through
    /// an API, without a download or its page ([`tells_size`]).
    pub tells_size: bool,
    /// How long its page, passed by itself, takes on its own before it could
    /// want a person ([`page_takes`]).
    pub page_takes: Duration,
    /// What its account does for it, as the Hosts view says it; empty for
    /// the usual words.
    pub account_note: &'static str,
}

/// What a host's file asks of a person without the host's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asks {
    Nothing,
    /// A captcha or a wait for every file.
    Always,
    /// A step on its page, unless the desktop app is there to pass it.
    OnItsPage,
}

impl Known {
    /// A wrapper's site, which stands for further links rather than files.
    pub fn wraps(&self) -> bool {
        self.wraps
    }

    /// Whether the user set every account setting this host takes.
    pub fn has_account(&self, cfg: &Config) -> bool {
        !self.accounts.is_empty()
            && self
                .accounts
                .iter()
                .all(|field| cfg.account(field.key).is_some())
    }

    /// It needs an account the user has not set.
    pub fn lacks_account(&self, cfg: &Config) -> bool {
        self.needs_account && !self.has_account(cfg)
    }

    /// It lacks its account, so its free download would cost the person a
    /// step for every file (a captcha, or a page the desktop app can't pass
    /// where `pages` is false), and the user did not ask to use it like any
    /// other host without one ([`Config::accountless_hosts`]): its links are
    /// used only when no other host has the file.
    pub fn last_resort(&self, cfg: &Config, pages: bool) -> bool {
        self.lacks_account(cfg)
            && !cfg.is_accountless(self.names[0])
            && asks_a_person(&format!("https://{}/", self.names[0]), cfg, pages)
    }

    /// The downloads at once it allows one address, unless an account
    /// lifts it.
    pub fn at_once(&self, cfg: &Config) -> Option<usize> {
        self.free.at_once.filter(|_| !self.has_account(cfg))
    }
}

/// A host's account, as the Hosts view asks for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AccountForm {
    pub host: String,
    /// What the account does for it.
    pub note: &'static str,
    pub fields: Vec<AccountField>,
    /// Every field is set.
    pub set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AccountField {
    /// Its name in the settings file.
    pub key: &'static str,
    pub label: &'static str,
    /// Hidden as it is typed.
    pub secret: bool,
}

/// The account form of every host that takes one, in the order of
/// [`KNOWN`].
pub fn account_forms(cfg: &Config) -> Vec<AccountForm> {
    KNOWN
        .iter()
        .filter(|k| !k.accounts.is_empty())
        .map(|k| AccountForm {
            host: k.names[0].to_string(),
            note: match k.account_note {
                "" if k.needs_account => {
                    "A premium account: without one, every file asks for a captcha."
                }
                note => note,
            },
            fields: k.accounts.to_vec(),
            set: k.has_account(cfg),
        })
        .collect()
}

/// A free user's limits, where none is known.
pub(super) const NO_LIMITS: Free = Free {
    at_once: None,
    daily_bytes: None,
    slowed_to: None,
    per_file: None,
};

/// What an entry of [`KNOWN`] leaves out. `names` and `site` are always its
/// own.
pub(super) const HOST: Known = Known {
    names: &[],
    site: &sites::Direct,
    wraps: false,
    accounts: &[],
    free: NO_LIMITS,
    tells_allowance: false,
    page_passes: false,
    needs_account: false,
    free_asks: Asks::Nothing,
    tells_size: false,
    page_takes: Duration::ZERO,
    account_note: "",
};

pub(super) const fn field(key: &'static str, label: &'static str, secret: bool) -> AccountField {
    AccountField { key, label, secret }
}

/// Every host with code of its own. Free limits are what was measured:
/// pixeldrain's from its own refusal ("maximum number of open download
/// connections for free/anonymous accounts", past 5) and its
/// `/api/misc/rate_limits`.
pub(super) static KNOWN: &[Known] = &[
    Known {
        names: &["pixeldrain.com"],
        site: &sites::Pixeldrain,
        accounts: &[field("pixeldrain_key", "API key", true)],
        free: Free {
            at_once: Some(5),
            daily_bytes: Some(6_000_000_000),
            slowed_to: Some(1_048_576),
            ..NO_LIMITS
        },
        tells_allowance: true,
        tells_size: true,
        account_note: "Its API key.",
        ..HOST
    },
    Known {
        names: &["gofile.io"],
        site: &sites::Gofile,
        accounts: &[field("gofile_token", "Account token", true)],
        free: Free {
            per_file: Some("Through its page, named late"),
            ..NO_LIMITS
        },
        page_passes: true,
        needs_account: true,
        free_asks: Asks::OnItsPage,
        account_note: "A premium account's token: without one, gofile's files come only through its page.",
        ..HOST
    },
    Known {
        names: &["buzzheavier.com"],
        site: &sites::Buzzheavier,
        ..HOST
    },
    Known {
        names: &["1fichier.com"],
        site: &sites::OneFichier,
        accounts: &[field("1fichier_key", "API key", true)],
        free: Free {
            per_file: Some("Wait and captcha per file"),
            ..NO_LIMITS
        },
        free_asks: Asks::Always,
        tells_size: true,
        account_note: "Its API key: no free-mode wait or captcha.",
        ..HOST
    },
    Known {
        names: &["rapidgator.net"],
        site: &sites::Rapidgator,
        accounts: &[
            field("rapidgator_user", "Username", false),
            field("rapidgator_pass", "Password", true),
        ],
        free: Free {
            per_file: Some("Wait and captcha per file"),
            ..NO_LIMITS
        },
        needs_account: true,
        free_asks: Asks::Always,
        ..HOST
    },
    Known {
        names: &["nitroflare.com"],
        site: &sites::Nitroflare,
        accounts: &[
            field("nitroflare_user", "User", false),
            field("nitroflare_key", "Premium key", true),
        ],
        free: Free {
            per_file: Some("Wait and captcha per file"),
            ..NO_LIMITS
        },
        needs_account: true,
        free_asks: Asks::Always,
        ..HOST
    },
    Known {
        names: &["mega.nz", "mega.co.nz", "transfer.it"],
        site: &sites::Mega,
        tells_size: true,
        ..HOST
    },
    Known {
        names: &["ddownload.com", "ddl.to"],
        site: &sites::Ddownload,
        // A 60-second wait and Cloudflare's Turnstile, which a browser
        // passes out of sight. One free file per address about every two
        // hours, which held at about 30 KB/s when tried (2026-11-05).
        free: Free {
            per_file: Some("1 file per 2 hours, about 30 KB/s"),
            ..NO_LIMITS
        },
        page_passes: true,
        free_asks: Asks::OnItsPage,
        // Everyone waits a minute first.
        page_takes: Duration::from_secs(75),
        ..HOST
    },
    Known {
        names: &["mediafire.com"],
        site: &sites::Mediafire,
        // Some files' pages are behind Cloudflare's check, whose clearance
        // does not let Haul's own requests through: the browser that passes
        // it takes the address on the page's download button.
        page_passes: true,
        tells_size: true,
        ..HOST
    },
    Known {
        names: &["rootz.so"],
        site: &sites::Rootz,
        // One download per address every two minutes.
        free: Free {
            at_once: Some(1),
            ..NO_LIMITS
        },
        tells_size: true,
        ..HOST
    },
    Known {
        names: &["vikingfile.com", "vik1ngfile.site"],
        site: &sites::Vikingfile,
        page_passes: true,
        free_asks: Asks::OnItsPage,
        tells_size: true,
        // It may take its time generating the link, which its page's
        // recipe gives 45 seconds before it tries again later.
        page_takes: Duration::from_secs(30),
        ..HOST
    },
    Known {
        names: &["fileditchfiles.st"],
        site: &sites::Direct,
        ..HOST
    },
    Known {
        names: &["fileguard.cc"],
        site: &sites::Fileguard,
        wraps: true,
        free: Free {
            per_file: Some("Proof of work, answered by Haul"),
            ..NO_LIMITS
        },
        ..HOST
    },
    Known {
        names: &["filecrypt.cc"],
        site: &sites::Filecrypt,
        wraps: true,
        free: Free {
            per_file: Some("Checkbox, answered by Haul"),
            ..NO_LIMITS
        },
        ..HOST
    },
    Known {
        names: &["mirrored.to"],
        site: &sites::Mirrored,
        wraps: true,
        ..HOST
    },
];

/// Every host Haul knows.
pub fn all() -> impl Iterator<Item = &'static Known> {
    KNOWN.iter()
}

/// Whether Haul runs as though it knew no site at all (`HAUL_GENERIC_ONLY`
/// set when it starts): every host, wrapper and paste gets the general way, so
/// the scorecard can measure what a host Haul has never seen would get. A
/// measurement switch, not a setting.
pub fn generic_only() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("HAUL_GENERIC_ONLY").is_some_and(|v| !v.is_empty() && v != "0")
    });
    *ON
}

/// The known host a host name (as [`host_of`] gives it) belongs to.
pub fn known(host: &str) -> Option<&'static Known> {
    if generic_only() {
        return None;
    }
    KNOWN.iter().find(|k| k.names.contains(&host))
}

/// How long a page that passes by itself takes on its own before it could
/// want a person ([`Known::page_takes`]).
pub fn page_takes(url: &str) -> Duration {
    known(&host_of(url)).map_or(Duration::ZERO, |k| k.page_takes)
}

/// Whether `url`'s page passes its check by itself (see
/// [`Known::page_passes`]).
pub fn page_passes(url: &str) -> bool {
    known(&host_of(url)).is_some_and(|k| k.page_passes)
}

/// Whether a file on `url`'s host takes a person, as far as can be told before
/// asking the host: a captcha for every file without its account, or a page
/// only the desktop app passes by itself (`pages`: it is there to).
pub fn asks_a_person(url: &str, cfg: &Config, pages: bool) -> bool {
    let Some(k) = known(&host_of(url)) else {
        return false;
    };
    if k.has_account(cfg) {
        return false;
    }
    match k.free_asks {
        Asks::Always => true,
        Asks::OnItsPage => !pages,
        Asks::Nothing => false,
    }
}

/// The known site a link is on.
pub(super) fn known_of(url: &str) -> Option<&'static Known> {
    known(&host_of(url))
}

/// What an address may still fetch at full speed today, as a host that
/// tells says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Allowance {
    #[serde(rename = "transfer_limit")]
    pub limit: u64,
    #[serde(rename = "transfer_limit_used")]
    pub used: u64,
    /// Bytes per second once `used` passes `limit`.
    #[serde(rename = "speed_limit")]
    pub slowed_to: u64,
}

impl Allowance {
    /// Past the day's full-speed bytes.
    pub fn slowed(&self) -> bool {
        self.limit > 0 && self.used >= self.limit
    }
}
