//! The code behind each site Haul knows, said once per site: what it
//! resolves, looks up, unwraps and tells. [`Known::site`] points at one of
//! these; what a site cannot do keeps the default.

use std::{future::Future, pin::Pin};

use super::*;

pub(super) type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The code behind a known site.
pub(super) trait Site: Sync + std::fmt::Debug {
    /// Direct links for one of its links.
    fn resolve<'a>(
        &self,
        cx: &'a Cx<'a>,
        url: &'a str,
    ) -> Fut<'a, Result<Vec<Entry>, ResolveError>>;

    /// What it says of a file without a download; by default, what any web
    /// server says.
    fn look<'a>(&self, cx: &'a Cx<'a>, url: &'a str) -> Fut<'a, Look> {
        Box::pin(async move { generic::probe(cx, url).await.into() })
    }

    /// The files behind one of its links, for a wrapper.
    fn unwrap<'a>(
        &self,
        _cx: &'a Cx<'a>,
        _url: &'a str,
    ) -> Fut<'a, Result<Unwrapped, ResolveError>> {
        Box::pin(async { Err(ResolveError::failed("not a wrapper link")) })
    }

    /// What two links to the same wrapper share.
    fn key(&self, _url: &str) -> Option<String> {
        None
    }

    /// A wrapper's page as a browser shows it, read for what it holds.
    fn shown(&self, _html: &str) -> Option<Unwrapped> {
        None
    }

    /// How much the address `cx` reaches it from may still fetch today.
    fn allowance<'a>(&self, _cx: &'a Cx<'a>) -> Fut<'a, Option<Allowance>> {
        Box::pin(async { None })
    }
}

/// A host whose links are the files.
#[derive(Debug)]
pub(super) struct Direct;

impl Site for Direct {
    fn resolve<'a>(
        &self,
        _cx: &'a Cx<'a>,
        url: &'a str,
    ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
        Box::pin(async move {
            Ok(vec![Entry {
                url: url.into(),
                name: guess_name(url),
                ..Entry::default()
            }])
        })
    }
}

/// A host whose resolver is all it has; lookups are a web server's.
macro_rules! resolves {
    ($site:ident, $module:ident) => {
        #[derive(Debug)]
        pub(super) struct $site;

        impl Site for $site {
            fn resolve<'a>(
                &self,
                cx: &'a Cx<'a>,
                url: &'a str,
            ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
                Box::pin($module::resolve(cx, url))
            }
        }
    };
}

resolves!(Buzzheavier, buzzheavier);
resolves!(Rapidgator, rapidgator);
resolves!(Nitroflare, nitroflare);

/// A host that can say a file was deleted before its turn.
macro_rules! looks {
    ($site:ident, $module:ident) => {
        #[derive(Debug)]
        pub(super) struct $site;

        impl Site for $site {
            fn resolve<'a>(
                &self,
                cx: &'a Cx<'a>,
                url: &'a str,
            ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
                Box::pin($module::resolve(cx, url))
            }

            fn look<'a>(&self, cx: &'a Cx<'a>, url: &'a str) -> Fut<'a, Look> {
                Box::pin($module::look(cx, url))
            }
        }
    };
}

looks!(Vikingfile, vikingfile);
looks!(Rootz, rootz);
looks!(Ddownload, ddownload);
looks!(OneFichier, onefichier);
looks!(Mega, mega);

#[derive(Debug)]
pub(super) struct Pixeldrain;

impl Site for Pixeldrain {
    fn resolve<'a>(
        &self,
        cx: &'a Cx<'a>,
        url: &'a str,
    ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
        Box::pin(pixeldrain::resolve(cx, url))
    }

    fn look<'a>(&self, cx: &'a Cx<'a>, url: &'a str) -> Fut<'a, Look> {
        Box::pin(pixeldrain::look(cx, url))
    }

    fn allowance<'a>(&self, cx: &'a Cx<'a>) -> Fut<'a, Option<Allowance>> {
        Box::pin(pixeldrain::allowance(cx))
    }
}

#[derive(Debug)]
pub(super) struct Mediafire;

impl Site for Mediafire {
    fn resolve<'a>(
        &self,
        cx: &'a Cx<'a>,
        url: &'a str,
    ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
        Box::pin(mediafire::resolve(cx, url))
    }

    fn look<'a>(&self, cx: &'a Cx<'a>, url: &'a str) -> Fut<'a, Look> {
        Box::pin(async move { mediafire::probe(cx, url).await.into() })
    }
}

#[derive(Debug)]
pub(super) struct Gofile;

impl Site for Gofile {
    fn resolve<'a>(
        &self,
        cx: &'a Cx<'a>,
        url: &'a str,
    ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
        Box::pin(gofile::resolve(cx, url))
    }

    /// No free metadata route: it is resolved at download time.
    fn look<'a>(&self, _cx: &'a Cx<'a>, _url: &'a str) -> Fut<'a, Look> {
        Box::pin(async { Look::Unknown })
    }
}

/// A wrapper: its links stand for further links. It says nothing useful
/// before its gate is passed, which is the unwrapping's work, once.
macro_rules! wrapper {
    ($site:ident, $module:ident $(, shown: $shown:path)?) => {
        #[derive(Debug)]
pub(super) struct $site;

        impl Site for $site {
            fn resolve<'a>(
                &self,
                cx: &'a Cx<'a>,
                url: &'a str,
            ) -> Fut<'a, Result<Vec<Entry>, ResolveError>> {
                Box::pin(async move { $module::unwrap(cx, url).await.map(|u| u.files) })
            }

            fn look<'a>(&self, _cx: &'a Cx<'a>, _url: &'a str) -> Fut<'a, Look> {
                Box::pin(async { Look::Unknown })
            }

            fn unwrap<'a>(
                &self,
                cx: &'a Cx<'a>,
                url: &'a str,
            ) -> Fut<'a, Result<Unwrapped, ResolveError>> {
                Box::pin($module::unwrap(cx, url))
            }

            fn key(&self, url: &str) -> Option<String> {
                $module::key(url)
            }

            $(
                fn shown(&self, html: &str) -> Option<Unwrapped> {
                    $shown(html)
                }
            )?
        }
    };
}

wrapper!(Fileguard, fileguard);
wrapper!(Filecrypt, filecrypt, shown: filecrypt::shown);
wrapper!(Mirrored, mirrored);
