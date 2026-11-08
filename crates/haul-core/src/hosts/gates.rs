//! The gates sites put in front of a file, recognised from what the page
//! shows, whichever site it is on. Most blocks are the same few products used
//! on many sites, so knowing one by its own marks works on every site that
//! uses it, and the row can say what stands in the way instead of "a page".
//! Recognising a gate answers nothing by itself: one Haul has no answer for
//! goes to a person, as before.

/// A gate, by the product behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Cloudflare's Turnstile box, which a real browser usually passes on its
    /// own.
    Turnstile,
    /// Google's reCAPTCHA.
    Recaptcha,
    /// hCaptcha.
    Hcaptcha,
    /// An ALTCHA proof of work, which a client can compute.
    Altcha,
    /// Friendly Captcha's proof of work.
    FriendlyCaptcha,
    /// GeeTest's puzzle.
    Geetest,
    /// CutCaptcha's puzzle.
    Cutcaptcha,
}

impl Gate {
    /// Whether a real browser gets past it with nobody's help: Turnstile
    /// usually lets one through, and a proof of work is the browser's to
    /// compute. A picture or a puzzle needs a person.
    pub fn passes_in_browser(self) -> bool {
        matches!(self, Self::Turnstile | Self::Altcha | Self::FriendlyCaptcha)
    }

    /// Its name, as the row says it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Turnstile => "Cloudflare Turnstile",
            Self::Recaptcha => "reCAPTCHA",
            Self::Hcaptcha => "hCaptcha",
            Self::Altcha => "an ALTCHA proof of work",
            Self::FriendlyCaptcha => "Friendly Captcha",
            Self::Geetest => "a GeeTest puzzle",
            Self::Cutcaptcha => "a CutCaptcha puzzle",
        }
    }
}

/// Each gate and the marks its widget leaves in a page: its script's
/// address, its element or the field it fills in. Lower-case.
const MARKS: &[(Gate, &[&str])] = &[
    (
        Gate::Turnstile,
        &["challenges.cloudflare.com/turnstile", "cf-turnstile"],
    ),
    (
        Gate::Recaptcha,
        &[
            "google.com/recaptcha",
            "recaptcha.net/recaptcha",
            "g-recaptcha",
            "grecaptcha",
        ],
    ),
    (
        Gate::Hcaptcha,
        &["hcaptcha.com/1/api.js", "h-captcha", "hcaptcha-response"],
    ),
    (
        Gate::Altcha,
        &["<altcha-widget", "altcha.min.js", "name=\"altcha\""],
    ),
    (Gate::FriendlyCaptcha, &["frc-captcha", "friendlycaptcha"]),
    (Gate::Geetest, &["geetest", "gt4.js", "initgeetest"]),
    (Gate::Cutcaptcha, &["cutcaptcha"]),
];

/// The gate `page` shows, when it shows one Haul knows. A page's own code
/// may name products it does not show (a site that can use several keeps
/// code for each), so marks in its markup, a script it loads or an element,
/// come before words in its inline code and comments: filecrypt.cc's page
/// loads CutCaptcha while its code still names reCAPTCHA (2026-11-08).
pub fn detect(page: &str) -> Option<Gate> {
    let page = page.to_ascii_lowercase();
    first_marked(&markup(&page)).or_else(|| first_marked(&page))
}

/// Each gate whose marks `page` holds, with the marks found, in order: what
/// a log shows when a page names more than one.
pub fn marks_in(page: &str) -> Vec<(Gate, Vec<&'static str>)> {
    let page = page.to_ascii_lowercase();
    MARKS
        .iter()
        .filter_map(|(gate, marks)| {
            let found: Vec<_> = marks.iter().copied().filter(|m| page.contains(m)).collect();
            (!found.is_empty()).then_some((*gate, found))
        })
        .collect()
}

fn first_marked(page: &str) -> Option<Gate> {
    MARKS
        .iter()
        .find(|(_, marks)| marks.iter().any(|m| page.contains(m)))
        .map(|(gate, _)| *gate)
}

/// Lower-case `page` without its inline scripts' code and its comments; a
/// script's own tag, which holds its address, stays.
fn markup(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    let mut rest = page;
    loop {
        let comment = rest.find("<!--");
        let Some(at) = rest.find("<script").into_iter().chain(comment).min() else {
            break;
        };
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if comment == Some(at) {
            rest = rest.find("-->").map_or("", |i| &rest[i + 3..]);
            continue;
        }
        let open = rest.find('>').map_or(rest.len(), |i| i + 1);
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        rest = rest.find("</script").map_or("", |i| &rest[i..]);
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_gate_is_known_by_its_own_marks() {
        let cases = [
            (
                r#"<div class="cf-turnstile" data-sitekey="x"></div>"#,
                Gate::Turnstile,
            ),
            (
                r#"<script src="https://www.google.com/recaptcha/api.js"></script>"#,
                Gate::Recaptcha,
            ),
            (r#"<div class="g-recaptcha"></div>"#, Gate::Recaptcha),
            (
                r#"<script src="https://js.hcaptcha.com/1/api.js"></script>"#,
                Gate::Hcaptcha,
            ),
            (
                r#"<altcha-widget challengeurl="/c"></altcha-widget>"#,
                Gate::Altcha,
            ),
            (
                r#"<div class="frc-captcha" data-sitekey="x"></div>"#,
                Gate::FriendlyCaptcha,
            ),
            (r#"<script>initGeetest({})</script>"#, Gate::Geetest),
            (
                r#"<script src="https://cdn.cutcaptcha.net/captcha/abc123.js"></script>"#,
                Gate::Cutcaptcha,
            ),
            (
                r#"<script>var CUTCAPTCHA_MISERY_KEY = "abc123";</script>"#,
                Gate::Cutcaptcha,
            ),
        ];
        for (page, gate) in cases {
            assert_eq!(detect(page), Some(gate), "{page}");
        }
    }

    #[test]
    fn what_a_page_loads_comes_before_what_its_code_names() {
        let page = r#"<script>if (window.grecaptcha) { grecaptcha.reset(); }</script>
            <!-- <div class="g-recaptcha"></div> -->
            <script src="https://api.cutcaptcha.net/v1/api.js"></script>
            <div id="puzzle-captcha"></div>"#;
        assert_eq!(detect(page), Some(Gate::Cutcaptcha));
        assert_eq!(
            marks_in(page),
            [
                (Gate::Recaptcha, vec!["g-recaptcha", "grecaptcha"]),
                (Gate::Cutcaptcha, vec!["cutcaptcha"]),
            ]
        );
        // Named only in code, a gate is still found.
        assert_eq!(
            detect(r#"<script>var CUTCAPTCHA_MISERY_KEY = "abc123";</script>"#),
            Some(Gate::Cutcaptcha)
        );
        // An unclosed script or comment ends the markup.
        assert_eq!(
            detect("<div class=\"g-recaptcha\"></div><script>cutcaptcha"),
            Some(Gate::Recaptcha)
        );
        assert_eq!(detect("<!-- grecaptcha"), Some(Gate::Recaptcha));
    }

    #[test]
    fn a_page_without_a_gate_is_none() {
        assert_eq!(
            detect("<title>Download Example</title><a href='/file'>Download</a>"),
            None
        );
    }
}
