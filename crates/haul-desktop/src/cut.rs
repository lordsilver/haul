//! The CutCaptcha widget on a page being passed: a checkbox that most
//! visits pass by ticking it, or without even that, and a jigsaw for the
//! visits its service doubts ([`haul_core::jigsaw`]). filecrypt.cc puts it
//! in front of a container once an address has read a few.
//!
//! The widget lives in closed shadow roots, which a page's scripts cannot
//! look into and DevTools can: its parts are found by their classes in
//! the document as DevTools gives it, and pressed and dragged with the
//! mouse events a person's hand makes.
//!
//! In turn, each a few seconds apart: wait a moment for the widget to pass
//! by itself; tick its checkbox, with whatever frame an advert laid over
//! it out of the way; when the jigsaw opens, read its picture and pieces
//! from the widget, and drag each piece to the hole it came from when the
//! solver is sure, or ask the widget for another picture when it is not.

use std::time::{Duration, Instant};

use haul_core::jigsaw::{self, made_up::Dice};
use serde_json::{Value, json};

use super::Cdp;

/// What the pass has done with the page's widget so far.
#[derive(Default)]
pub struct Widget {
    /// When the widget was first seen waiting for its answer.
    seen: Option<Instant>,
    /// The last thing done to it, and when.
    acted: Option<Instant>,
    ticks: u32,
    reloads: u32,
    verifies: u32,
}

/// The widget passes by itself within this, when it is going to.
const BY_ITSELF: Duration = Duration::from_secs(6);
/// Between two things done to the widget: its answer to the last comes
/// first.
const EVERY: Duration = Duration::from_millis(2500);
/// The checkbox is ticked again after this long without a jigsaw or an
/// answer, once.
const TICK_AGAIN: Duration = Duration::from_secs(15);
const TICKS: u32 = 2;
/// Other pictures asked for, and answers given, at most.
const RELOADS: u32 = 5;
const VERIFIES: u32 = 3;

/// Whether the page has the widget, still without its answer.
const WAITS: &str = r"(() => {
  const answer = document.querySelector('.cutcaptcha [name=cutcaptcha-response]');
  return Boolean(answer) && !answer.value;
})()";

/// Lets clicks through whatever frame lies over the page at `X`, `Y`: an
/// advert's, laid over the widget to take the click meant for it.
const UNCOVER: &str = r"((x, y) => {
  let moved = 0;
  for (let i = 0; i < 6; i++) {
    const over = document.elementFromPoint(x, y);
    if (!over || over.tagName !== 'IFRAME') break;
    over.style.setProperty('pointer-events', 'none', 'important');
    moved++;
  }
  return moved;
})(X, Y)";

/// A box on the page: left, top, width, height.
type Area = (f64, f64, f64, f64);

/// The widget's parts as the document holds them.
#[derive(Debug, Default, PartialEq)]
pub struct Seen {
    /// The checkbox, by its node.
    pub checkbox: Option<i64>,
    /// The jigsaw, when it is open.
    pub jigsaw: Option<Jigsaw>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Jigsaw {
    pub stage: i64,
    pub verify: i64,
    pub reload: i64,
    /// The picture's `data:` address and the width it is drawn at.
    pub picture: String,
    pub width: f32,
    pub pieces: Vec<PieceSeen>,
}

/// A piece: its image's `data:` address, and where its box sits in the
/// stage, in the page's pixels.
#[derive(Debug, Default, PartialEq)]
pub struct PieceSeen {
    pub image: String,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

fn attribute<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    let all = node["attributes"].as_array()?;
    all.chunks(2)
        .find(|pair| pair[0].as_str() == Some(name))
        .and_then(|pair| pair.get(1)?.as_str())
}

fn has_class(node: &Value, class: &str) -> bool {
    attribute(node, "class").is_some_and(|c| c.split_whitespace().any(|c| c == class))
}

/// Every node under `node` with class `class`, through shadow roots and
/// frames, in the document's order.
fn with_class<'a>(node: &'a Value, class: &str, found: &mut Vec<&'a Value>) {
    if has_class(node, class) {
        found.push(node);
    }
    for key in ["children", "shadowRoots"] {
        for child in node[key].as_array().into_iter().flatten() {
            with_class(child, class, found);
        }
    }
    if node["contentDocument"].is_object() {
        with_class(&node["contentDocument"], class, found);
    }
}

fn first<'a>(node: &'a Value, class: &str) -> Option<&'a Value> {
    let mut found = Vec::new();
    with_class(node, class, &mut found);
    found.first().copied()
}

/// The pixels `key` is set to in a `style` attribute.
fn pixels(style: &str, key: &str) -> Option<f32> {
    style.split(';').find_map(|part| {
        let (name, value) = part.split_once(':')?;
        (name.trim() == key).then(|| value.trim().trim_end_matches("px").parse().ok())?
    })
}

/// The widget in `document` (DevTools' `DOM.getDocument`, pierced).
pub fn seen(document: &Value) -> Seen {
    let root = &document["root"];
    let checkbox = first(root, "cc__box").and_then(|n| n["nodeId"].as_i64());
    let jigsaw = (|| {
        let stage = first(root, "ccm__stage")?;
        let picture = first(stage, "ccm__pic")?;
        let mut boxes = Vec::new();
        with_class(stage, "ccm__piece", &mut boxes);
        let mut pieces = Vec::new();
        for piece in boxes {
            let style = attribute(piece, "style")?;
            let image = piece["children"]
                .as_array()?
                .iter()
                .find(|c| c["nodeName"].as_str() == Some("IMG"))?;
            pieces.push(PieceSeen {
                image: attribute(image, "src")?.to_string(),
                left: pixels(style, "left")?,
                top: pixels(style, "top")?,
                width: pixels(style, "width")?,
                height: pixels(style, "height")?,
            });
        }
        Some(Jigsaw {
            stage: stage["nodeId"].as_i64()?,
            verify: first(root, "ccm__go")?["nodeId"].as_i64()?,
            reload: first(root, "ccm__re")?["nodeId"].as_i64()?,
            picture: attribute(picture, "src")?.to_string(),
            width: attribute(picture, "width")?.parse().ok()?,
            pieces,
        })
    })();
    Seen { checkbox, jigsaw }
}

impl Widget {
    /// The time a widget being answered adds to its pass.
    pub fn takes(&self) -> Duration {
        if self.seen.is_some() {
            Duration::from_secs(75)
        } else {
            Duration::ZERO
        }
    }

    /// Whether the widget is on the page without its answer, as last seen:
    /// the pass leaves the page alone meanwhile.
    pub fn waits(&self) -> bool {
        self.seen.is_some()
    }

    /// One turn with the page's widget, when it has one that still waits:
    /// what was done, for the signals.
    pub fn step(&mut self, cdp: &mut Cdp, s: Option<&str>) -> Option<Value> {
        let now = Instant::now();
        if self.acted.is_some_and(|at| now.duration_since(at) < EVERY) {
            return None;
        }
        let waits = cdp
            .call(
                "Runtime.evaluate",
                json!({ "expression": WAITS, "returnByValue": true }),
                s,
            )
            .ok()?["result"]["value"]
            .as_bool()?;
        if !waits {
            self.seen = None;
            return None;
        }
        let since = *self.seen.get_or_insert(now);
        if now.duration_since(since) < BY_ITSELF {
            return None;
        }
        let document = cdp
            .call("DOM.getDocument", json!({ "depth": -1, "pierce": true }), s)
            .ok()?;
        let seen = seen(&document);
        if let Some(jigsaw) = seen.jigsaw {
            return self.answer(cdp, s, &jigsaw);
        }
        let again = self
            .acted
            .is_none_or(|at| now.duration_since(at) >= TICK_AGAIN);
        if self.ticks >= TICKS || !again {
            return None;
        }
        let (x, y, _, h) = area(cdp, s, seen.checkbox?)?;
        let (x, y) = (x + 30.0, y + h / 2.0);
        uncover(cdp, s, x, y);
        let mut dice = Dice::new(now.elapsed().as_nanos() as u64 ^ 0x51ed);
        press(cdp, s, &mut dice, x, y);
        self.ticks += 1;
        self.acted = Some(Instant::now());
        Some(json!({ "step": "checkbox", "try": self.ticks }))
    }

    /// The open jigsaw: its pieces dragged home and Verify pressed, or
    /// another picture asked for.
    fn answer(&mut self, cdp: &mut Cdp, s: Option<&str>, jigsaw: &Jigsaw) -> Option<Value> {
        if jigsaw.pieces.is_empty() || self.verifies >= VERIFIES {
            return None;
        }
        let puzzle = jigsaw::Puzzle {
            picture: jigsaw::data_uri(&jigsaw.picture)?,
            width: jigsaw.width,
            pieces: jigsaw
                .pieces
                .iter()
                .map(|p| {
                    Some(jigsaw::Piece {
                        image: jigsaw::data_uri(&p.image)?,
                        width: p.width,
                    })
                })
                .collect::<Option<_>>()?,
        };
        let began = Instant::now();
        let answer = jigsaw::solve(&puzzle);
        let took = began.elapsed().as_millis() as u64;
        let mut dice = Dice::new(began.elapsed().as_nanos() as u64 ^ 0x9d2c);
        let placed = match answer {
            Ok(jigsaw::Answer::Sure(placed)) => placed,
            Ok(jigsaw::Answer::Unsure(why)) | Err(why) => {
                if self.reloads >= RELOADS {
                    return None;
                }
                let (x, y, w, h) = area(cdp, s, jigsaw.reload)?;
                uncover(cdp, s, x + w / 2.0, y + h / 2.0);
                press(cdp, s, &mut dice, x + w / 2.0, y + h / 2.0);
                self.reloads += 1;
                self.acted = Some(Instant::now());
                return Some(json!({ "step": "another picture", "why": why, "solver_ms": took }));
            }
        };
        let (stage_x, stage_y, _, _) = area(cdp, s, jigsaw.stage)?;
        for put in &placed {
            let piece = jigsaw.pieces.get(put.piece)?;
            let from = (
                stage_x + f64::from(piece.left + piece.width / 2.0),
                stage_y + f64::from(piece.top + piece.height / 2.0),
            );
            let to = (
                from.0 + f64::from(put.left - piece.left),
                from.1 + f64::from(put.top - piece.top),
            );
            uncover(cdp, s, from.0, from.1);
            drag(cdp, s, &mut dice, from, to);
        }
        let (x, y, w, h) = area(cdp, s, jigsaw.verify)?;
        uncover(cdp, s, x + w / 2.0, y + h / 2.0);
        press(cdp, s, &mut dice, x + w / 2.0, y + h / 2.0);
        self.verifies += 1;
        self.acted = Some(Instant::now());
        Some(json!({
            "step": "jigsaw",
            "try": self.verifies,
            "pieces": placed.len(),
            "fit": placed.iter().map(|p| (p.fit * 100.0).round() / 100.0).collect::<Vec<_>>(),
            "solver_ms": took,
        }))
    }
}

/// Where node `node` is drawn on the page.
fn area(cdp: &mut Cdp, s: Option<&str>, node: i64) -> Option<Area> {
    let model = cdp
        .call("DOM.getBoxModel", json!({ "nodeId": node }), s)
        .ok()?;
    let q: Vec<f64> = model["model"]["border"]
        .as_array()?
        .iter()
        .filter_map(Value::as_f64)
        .collect();
    (q.len() == 8).then(|| (q[0], q[1], q[2] - q[0], q[5] - q[1]))
}

fn uncover(cdp: &mut Cdp, s: Option<&str>, x: f64, y: f64) {
    let script = UNCOVER
        .replace('X', &x.to_string())
        .replace('Y', &y.to_string());
    let _ = cdp.call(
        "Runtime.evaluate",
        json!({ "expression": script, "returnByValue": true }),
        s,
    );
}

fn mouse(cdp: &mut Cdp, s: Option<&str>, kind: &str, x: f64, y: f64, held: bool) {
    let pressing = kind != "mouseMoved";
    let _ = cdp.call(
        "Input.dispatchMouseEvent",
        json!({
            "type": kind,
            "x": x,
            "y": y,
            "button": if pressing || held { "left" } else { "none" },
            "buttons": i32::from(held),
            "clickCount": i32::from(pressing),
        }),
        s,
    );
}

fn rest(dice: &mut Dice, from_ms: f32, to_ms: f32) {
    std::thread::sleep(Duration::from_millis(dice.between(from_ms, to_ms) as u64));
}

/// The pointer comes to `x`, `y` from a little way off, and clicks.
fn press(cdp: &mut Cdp, s: Option<&str>, dice: &mut Dice, x: f64, y: f64) {
    let from = (
        x + f64::from(dice.between(40.0, 140.0)),
        y + f64::from(dice.between(20.0, 70.0)),
    );
    glide(cdp, s, dice, from, (x, y), false);
    rest(dice, 80.0, 180.0);
    mouse(cdp, s, "mousePressed", x, y, true);
    rest(dice, 60.0, 120.0);
    mouse(cdp, s, "mouseReleased", x, y, false);
}

/// The pointer moves from `from` to `to` as a hand does: slow at both
/// ends, along a slight bow, never quite steady.
fn glide(
    cdp: &mut Cdp,
    s: Option<&str>,
    dice: &mut Dice,
    from: (f64, f64),
    to: (f64, f64),
    held: bool,
) {
    let steps = dice.between(16.0, 26.0) as u32;
    let bow = f64::from(dice.between(-16.0, 16.0));
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        let eased = t * t * (3.0 - 2.0 * t);
        let arc = (std::f64::consts::PI * t).sin() * bow;
        let (mut x, mut y) = (
            from.0 + (to.0 - from.0) * eased + arc * 0.3 + f64::from(dice.between(-0.6, 0.6)),
            from.1 + (to.1 - from.1) * eased + arc + f64::from(dice.between(-0.6, 0.6)),
        );
        if step == steps {
            (x, y) = to;
        }
        mouse(cdp, s, "mouseMoved", x, y, held);
        rest(dice, 10.0, 30.0);
    }
}

/// A piece taken at `from` and let go at `to`.
fn drag(cdp: &mut Cdp, s: Option<&str>, dice: &mut Dice, from: (f64, f64), to: (f64, f64)) {
    let near = (
        from.0 - f64::from(dice.between(20.0, 60.0)),
        from.1 + f64::from(dice.between(15.0, 40.0)),
    );
    glide(cdp, s, dice, near, from, false);
    rest(dice, 90.0, 200.0);
    mouse(cdp, s, "mousePressed", from.0, from.1, true);
    rest(dice, 80.0, 160.0);
    glide(cdp, s, dice, from, to, true);
    rest(dice, 120.0, 260.0);
    mouse(cdp, s, "mouseReleased", to.0, to.1, false);
    rest(dice, 250.0, 500.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, name: &str, attributes: &[&str], children: Vec<Value>) -> Value {
        json!({ "nodeId": id, "nodeName": name, "attributes": attributes, "children": children })
    }

    /// The widget's parts are found through its closed shadow roots, and a
    /// piece's place is read from its style.
    #[test]
    fn the_widget_is_read_from_the_document() {
        let piece = node(
            7,
            "DIV",
            &[
                "class",
                "ccm__piece",
                "style",
                "left: 27.25px; top: 341.75px; width: 130.9px; height: 130.9px;",
            ],
            vec![node(
                8,
                "IMG",
                &["src", "data:image/png;base64,AAEC"],
                vec![],
            )],
        );
        let stage = node(
            4,
            "DIV",
            &["class", "ccm__stage"],
            vec![
                node(
                    5,
                    "IMG",
                    &[
                        "class",
                        "ccm__pic",
                        "src",
                        "data:image/jpeg;base64,AAEC",
                        "width",
                        "480",
                    ],
                    vec![],
                ),
                piece,
            ],
        );
        let modal = json!({ "nodeId": 3, "nodeName": "DIV", "attributes": [], "shadowRoots": [node(
            9, "#document-fragment", &[], vec![
                stage,
                node(10, "BUTTON", &["class", "ccm__btn ccm__re"], vec![]),
                node(11, "BUTTON", &["class", "ccm__btn ccm__go"], vec![]),
            ],
        )] });
        let host = json!({ "nodeId": 2, "nodeName": "DIV", "attributes": [], "shadowRoots": [node(
            12, "#document-fragment", &[], vec![node(13, "DIV", &["class", "cc__box"], vec![])],
        )] });
        let document = json!({ "root": node(1, "#document", &[], vec![host, modal]) });
        let seen = seen(&document);
        assert_eq!(seen.checkbox, Some(13));
        let jigsaw = seen.jigsaw.expect("the open jigsaw");
        assert_eq!((jigsaw.stage, jigsaw.verify, jigsaw.reload), (4, 11, 10));
        assert_eq!(jigsaw.width, 480.0);
        assert_eq!(
            jigsaw.pieces,
            [PieceSeen {
                image: "data:image/png;base64,AAEC".into(),
                left: 27.25,
                top: 341.75,
                width: 130.9,
                height: 130.9,
            }]
        );
        // Before the jigsaw opens, only the checkbox.
        let document = json!({ "root": node(1, "#document", &[], vec![]) });
        assert_eq!(super::seen(&document), Seen::default());
    }
}
