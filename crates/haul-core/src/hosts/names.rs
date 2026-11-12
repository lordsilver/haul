//! Host and file names: which host a link is on, and a file's name from a link or a page.

use super::*;

/// Lower-case host name without port or `www.`; a known host's other
/// domain gives its main one.
pub fn host_of(url: &str) -> String {
    main_name(&domain_of(url))
}

/// A host's main domain for any of its domains ([`ALIASES`]); any other
/// name as it is, lowercase.
pub fn main_name(host: &str) -> String {
    let host = host.to_ascii_lowercase();
    match ALIASES.iter().find(|(alias, _)| *alias == host) {
        Some((_, name)) => (*name).to_string(),
        None => host,
    }
}

/// Other domains of a host, counted as its main one ([`host_of`]): its
/// settings, limits and speed are the host's, whichever domain a link is on.
pub(super) const ALIASES: &[(&str, &str)] = &[
    ("vik1ngfile.site", "vikingfile.com"),
    ("ddl.to", "ddownload.com"),
    ("mega.co.nz", "mega.nz"),
    ("rentry.org", "rentry.co"),
];

/// Every site Haul has code for, a host's other domains left out: its file
/// hosts, wrappers and pastebins, which the Hosts view lists whether a link
/// is on them yet or not.
pub fn known_sites() -> Vec<String> {
    let alias = |name: &&str| ALIASES.iter().any(|(a, _)| a == name);
    KNOWN
        .iter()
        .flat_map(|k| k.names.iter())
        .chain(generic::PASTES)
        .filter(|name| !alias(name))
        .map(|name| name.to_string())
        .collect()
}

/// A link's host name, lowercase and without `www.`, as it is written (a
/// stand-in's without its `.localhost`, [`super::stand_in`]).
pub(super) fn domain_of(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    let host = super::stand_in::real_name(&host);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// The site a link's host belongs to, as far as who runs it goes: the host
/// name's last two labels, or three under a country's second level
/// (`example.co.uk`); an address stays as it is.
pub fn site_of(url: &str) -> String {
    let host = domain_of(url);
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host;
    }
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    let n = labels.len();
    let country_second_level = n >= 3
        && labels[n - 1].len() == 2
        && matches!(
            labels[n - 2],
            "co" | "com" | "net" | "org" | "gov" | "edu" | "ac" | "or" | "ne" | "go" | "gob"
        );
    let keep = if country_second_level { 3 } else { 2 };
    labels[n.saturating_sub(keep)..].join(".")
}

pub(super) static FILE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.\w{2,5}$").unwrap());

/// The labels a source shows beside its links (`labels`, by place), kept
/// only where they are hints worth taking (plan §4.2 rule 4): one that reads
/// as a file's name, not a web page's (`Error 1020.html`), and, in a listing
/// whose labels are parts of one archive, a part of it. So an interstitial's
/// title (`Just a moment...`) or an error page's is dropped without a list
/// of bad words.
pub fn hint_labels(labels: Vec<Option<String>>) -> Vec<Option<String>> {
    use crate::parts::PartName;
    let file = |l: &str| {
        FILE_NAME.is_match(l) && super::page_extension_of(l).is_none() && !l.contains("://")
    };
    let labels: Vec<Option<String>> = labels
        .into_iter()
        .map(|l| l.map(|l| l.trim().to_string()).filter(|l| file(l)))
        .collect();
    // The archive most labels are parts of, when at least two are.
    let mut counts: std::collections::HashMap<(crate::parts::Form, String), usize> =
        std::collections::HashMap::new();
    for part in labels.iter().flatten().filter_map(|l| PartName::parse(l)) {
        *counts.entry(part.key()).or_default() += 1;
    }
    let archive = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .max_by_key(|(_, n)| *n)
        .map(|(k, _)| k);
    labels
        .into_iter()
        .map(|l| match &archive {
            Some(key) => l.filter(|l| PartName::parse(l).is_some_and(|p| p.key() == *key)),
            None => l,
        })
        .collect()
}

/// Extensions of archives, their parts, media and the like: what a page's
/// title, or a link after `#`, names a file by.
pub(crate) const FILE_EXTENSIONS: &str = r"rar|zip|7z|iso|exe|msi|bin|mkv|mp4|avi|mov|mp3|flac|pdf|epub|cbz|cbr|tar|gz|xz|bz2|apk|dmg|img|part\d+\.rar|r\d\d|z\d\d";

/// A file's name after a link's `#` (`#Archive.part01.rar`), not a key
/// (`#k3y`, `#!id!key`) or an anchor (`#comments`).
pub(super) static FRAGMENT_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)^[^/=!?]+\.(?:{FILE_EXTENSIONS})$")).unwrap());

/// The last part of a link's path, decoded.
pub(super) fn last_segment(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let last = url.path().trim_end_matches('/').rsplit('/').next()?;
    Some(
        percent_encoding::percent_decode_str(last)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

/// The file's id in `url`, as the first group of `link` finds it; a link it
/// does not find one in is not `host`'s file link.
pub(crate) fn link_id(link: &Regex, url: &str, host: &str) -> Result<String, ResolveError> {
    link.captures(url)
        .map(|c| c[1].to_string())
        .ok_or_else(|| ResolveError::failed(format!("{host}: not a file link: {url}")))
}

/// The name a link's file goes by: `given` when there is one, else the one
/// its address suggests ([`guess_name`]).
pub fn name_for(url: &str, given: Option<String>) -> Option<String> {
    given
        .filter(|n| !n.trim().is_empty())
        .or_else(|| guess_name(url))
}

impl Entry {
    /// What its file goes by ([`name_for`]).
    pub fn file_name(&self) -> Option<String> {
        name_for(&self.url, self.name.clone())
    }
}

/// The file name in a link's path, when it looks like one. File hosts' pages
/// often add `.html` to the name of the file they offer (`name.rar.html`),
/// which names `name.rar`; a page of its own (`index.php`) names no file.
/// A path that names none may have its file's name after `#`
/// (`…/abc123def456#Archive.part01.rar`).
pub fn guess_name(url: &str) -> Option<String> {
    let in_path = last_segment(url).and_then(|last| {
        let name = page_extension(&last).unwrap_or(&last);
        FILE_NAME.is_match(name).then(|| name.to_string())
    });
    in_path.or_else(|| {
        let fragment = url::Url::parse(url).ok()?.fragment()?.to_string();
        let name = percent_encoding::percent_decode_str(&fragment).decode_utf8_lossy();
        FRAGMENT_NAME.is_match(&name).then(|| name.into_owned())
    })
}

/// A page's text for a heading: entities decoded, `None` when empty.
pub(super) fn page_text(raw: &str) -> Option<String> {
    let text = decode_entities(raw.trim());
    (!text.is_empty()).then_some(text)
}

pub(super) static ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z]{2,6});").unwrap());

/// HTML's character references, named and numeric (filecrypt.cc writes a
/// name's brackets as `&#40;` and `&#41;`); an unknown one stays as it is.
/// The last part of a link's path, which several hosts use as the file's id.
pub(super) fn last_path_part(url: &str) -> &str {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

pub(super) fn decode_entities(text: &str) -> String {
    // Some pages escape a reference's own `&` too (`&amp;#40;`).
    let once = decode_once(text);
    if once.contains('&') {
        decode_once(&once)
    } else {
        once
    }
}

pub(super) fn decode_once(text: &str) -> String {
    ENTITY
        .replace_all(text, |c: &regex::Captures<'_>| {
            let body = &c[1];
            let ch = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match body {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "nbsp" => Some(' '),
                    _ => None,
                }
            };
            ch.map_or_else(|| c[0].to_string(), String::from)
        })
        .into_owned()
}

/// A browser's mark for a second copy of a file, ` (2)` before its
/// extension, as some uploads keep it.
pub(super) static COPY_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?) \(\d{1,3}\)(\.\w{2,5})$").unwrap());

/// A file name as Haul saves and shows it: character references decoded
/// and a copy mark (`name.part1 (2).rar`) taken off, since the parts of an
/// archive are only found by their plain names.
pub fn tidy_name(name: &str) -> String {
    let name = decode_entities(name.trim());
    match COPY_MARK.captures(&name) {
        Some(c) => format!("{}{}", &c[1], &c[2]),
        None => name,
    }
}

/// What a file name is recognised by on every host: [`tidy_name`], with
/// `_` for a space as some hosts write it, runs of spaces as one, and any
/// case.
pub fn name_key(name: &str) -> String {
    tidy_name(name)
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Of two names for the same file, the one to keep: the one a host wrote
/// with spaces rather than `_`, then the one without a copy mark.
pub fn better_name(a: String, b: String) -> String {
    let rank = |n: &str| {
        (
            n.matches('_').count(),
            COPY_MARK.is_match(n),
            n.contains('&'),
        )
    };
    if rank(&b) < rank(&a) { b } else { a }
}
