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
//! The service tells the widget, a second after the page loads, whether
//! this visit passes with no click ([`Heard`]). In turn: leave a widget
//! that passes by itself alone; tick the checkbox of one that does not, at
//! once, with whatever frame an advert laid over it out of the way (the
//! widget's proof of work, some seconds long, starts with the tick); when
//! the jigsaw opens, read its picture and pieces from the widget, and drag
//! each piece to the hole it came from when the solver is sure, or ask the
//! widget for another picture when it is not.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use haul_core::jigsaw::{self, made_up::Dice};
use serde_json::{Value, json};

use super::Cdp;

/// What the pass has done with the page's widget so far.
#[derive(Default)]
pub struct Widget {
    /// When the widget was first seen waiting for its answer.
    seen: Option<Instant>,
    /// When a widget that had not started was brought into view, once.
    shown: Option<Instant>,
    /// It had still not started when last looked at.
    unstarted: bool,
    /// The last thing done to it, and when.
    acted: Option<Instant>,
    /// It is left alone until then: its answer to the last thing done
    /// comes first.
    rests: Option<Instant>,
    ticks: u32,
    reloads: u32,
    verifies: u32,
    /// Times the page did not answer the pointer: after a few the widget
    /// is left alone.
    stalls: u32,
}

/// The widget passes by itself within this, when it is going to and its
/// service's word on that was not heard.
const BY_ITSELF: Duration = Duration::from_secs(6);
/// A widget whose service said it wants its checkbox ticked is ticked this
/// long after it shows, as soon as a person finds the box.
const TOLD_TO_TICK: Duration = Duration::from_millis(1500);
/// A widget whose service said it passes by itself has this long to: its
/// proof of work takes a few seconds, now and then many.
const TOLD_IT_PASSES: Duration = Duration::from_secs(25);
/// After a tick the jigsaw is looked for this often: it opens once the
/// widget's proof of work is done.
const AFTER_TICK: Duration = Duration::from_secs(1);
/// After anything else done to the widget: its answer comes first.
const EVERY: Duration = Duration::from_millis(2500);
/// The checkbox is ticked again after this long without a jigsaw or an
/// answer, once.
const TICK_AGAIN: Duration = Duration::from_secs(15);
const TICKS: u32 = 2;
/// A widget brought into view to start has by now, or will not.
const STARTS_WITHIN: Duration = Duration::from_secs(5);
/// Other pictures asked for, and answers given, at most.
const RELOADS: u32 = 5;
const VERIFIES: u32 = 3;
const STALLS: u32 = 3;

/// The page's widget: 0 when it has none, or one with its answer; 1 for one
/// that has not started (its place on the page still empty: some start only
/// once seen); 2 for one that waits for its answer.
const WAITS: &str = r"(() => {
  const widget = document.querySelector('.cutcaptcha');
  if (!widget) return 0;
  const answer = widget.querySelector('[name=cutcaptcha-response]');
  if (!answer) return 1;
  return answer.value ? 0 : 2;
})()";
const NOT_STARTED: i64 = 1;
const WAITING: i64 = 2;

/// Brings the widget into view: one further down the page is not drawn
/// where a pointer reaches it, and some start only once seen.
const INTO_VIEW: &str = r"(() => {
  const widget = document.querySelector('.cutcaptcha');
  if (!widget) return;
  const box = widget.getBoundingClientRect();
  if (box.top < 0 || box.bottom > innerHeight) widget.scrollIntoView({ block: 'center', behavior: 'instant' });
})()";

/// What the widget's service told it of this visit, heard on the network
/// as the browser got it: the answer to the widget's `/v1/challenge`,
/// `{"data": {"auto": …}}`. The page cannot tell it is listened to.
#[derive(Debug, Default)]
pub struct Heard {
    /// The request whose answer says it.
    request: Option<String>,
    /// The question DevTools was asked for that answer's body.
    asked: Option<u64>,
    /// Whether the widget passes with no click, once heard.
    pub by_itself: Option<bool>,
}

impl Heard {
    /// Whether `url` is where the widget asks its service about the visit.
    fn asks_service(url: &str) -> bool {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        path.contains("cutcaptcha") && path.ends_with("/v1/challenge")
    }

    /// A network event of the page: the request whose answer's body is to
    /// be asked for, when the service's answer has all come.
    pub fn finished(&mut self, event: &Value) -> Option<String> {
        let p = &event["params"];
        match event["method"].as_str()? {
            "Network.responseReceived"
                if p["type"].as_str() != Some("Preflight")
                    && p["response"]["url"]
                        .as_str()
                        .is_some_and(Self::asks_service) =>
            {
                // A new visit's answer: the last one's no longer holds.
                self.by_itself = None;
                self.request = p["requestId"].as_str().map(str::to_string);
                None
            }
            "Network.loadingFinished"
                if self.request.is_some() && p["requestId"].as_str() == self.request.as_deref() =>
            {
                self.request.take()
            }
            _ => None,
        }
    }

    /// DevTools was asked for the answer's body, as its question `id`.
    pub fn asked(&mut self, id: u64) {
        self.asked = Some(id);
    }

    /// An answer of DevTools': the service's word, when it is the body
    /// asked for.
    pub fn answer(&mut self, answer: &Value) {
        if self.asked.is_none() || answer["id"].as_u64() != self.asked {
            return;
        }
        self.asked = None;
        self.by_itself = answer["result"]["body"]
            .as_str()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .and_then(|said| said["data"]["auto"].as_bool());
    }
}

/// Lets clicks through the topmost thing at `X`, `Y` when it is not the
/// widget's: an advert's frame or layer, laid over the widget to take the
/// click meant for it. Whether it did.
const UNCOVER: &str = r"((x, y) => {
  const over = document.elementFromPoint(x, y);
  if (!over || over === document.documentElement || over === document.body) return false;
  over.style.setProperty('pointer-events', 'none', 'important');
  return true;
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
    /// Every node of the widget, by DevTools' lasting number for it: what
    /// its shadow roots hold, and their hosts.
    pub nodes: HashSet<i64>,
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

/// Every node under `node`, itself included, into `nodes`.
fn all_under(node: &Value, nodes: &mut HashSet<i64>) {
    nodes.extend(node["backendNodeId"].as_i64());
    for key in ["children", "shadowRoots"] {
        for child in node[key].as_array().into_iter().flatten() {
            all_under(child, nodes);
        }
    }
}

/// The hosts of the widget's shadow roots under `node` (those that hold
/// its checkbox or its jigsaw), with all they hold, into `nodes`.
fn widget_nodes(node: &Value, nodes: &mut HashSet<i64>) {
    let roots = node["shadowRoots"].as_array().into_iter().flatten();
    if roots
        .clone()
        .any(|r| first(r, "cc").is_some() || first(r, "ccm").is_some())
    {
        all_under(node, nodes);
        return;
    }
    for child in node["children"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(roots)
    {
        widget_nodes(child, nodes);
    }
}

/// The widget in `document` (DevTools' `DOM.getDocument`, pierced).
pub fn seen(document: &Value) -> Seen {
    let root = &document["root"];
    let mut nodes = HashSet::new();
    widget_nodes(root, &mut nodes);
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
    Seen {
        checkbox,
        jigsaw,
        nodes,
    }
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

    /// Whether the widget is on the page without its answer, as last seen,
    /// or was just brought into view to start: the pass leaves the page
    /// alone meanwhile.
    pub fn waits(&self) -> bool {
        self.seen.is_some()
            || self.unstarted && self.shown.is_some_and(|at| at.elapsed() < STARTS_WITHIN)
    }

    /// One turn with the page's widget, when it has one that still waits:
    /// what was done, for the signals.
    pub fn step(&mut self, cdp: &mut Cdp, s: Option<&str>) -> Option<Value> {
        let now = Instant::now();
        if self.stalls >= STALLS || self.rests.is_some_and(|until| now < until) {
            return None;
        }
        let waits = cdp
            .call(
                "Runtime.evaluate",
                json!({ "expression": WAITS, "returnByValue": true }),
                s,
            )
            .ok()?["result"]["value"]
            .as_i64()?;
        self.unstarted = waits == NOT_STARTED;
        let first = match waits {
            NOT_STARTED => {
                let first = self.shown.is_none();
                self.shown.get_or_insert(now);
                first
            }
            WAITING => self.seen.is_none(),
            _ => false,
        };
        if first {
            let _ = cdp.call("Runtime.evaluate", json!({ "expression": INTO_VIEW }), s);
        }
        if waits != WAITING {
            self.seen = None;
            return None;
        }
        let since = *self.seen.get_or_insert(now);
        // Before its first tick the widget is left to pass by itself, for
        // as long as its service's word on that says.
        let said = cdp.heard.by_itself;
        let patience = match said {
            Some(false) => TOLD_TO_TICK,
            Some(true) => TOLD_IT_PASSES,
            None => BY_ITSELF,
        };
        if self.ticks == 0 && now.duration_since(since) < patience {
            return None;
        }
        let document = cdp
            .call("DOM.getDocument", json!({ "depth": -1, "pierce": true }), s)
            .ok()?;
        let seen = seen(&document);
        let mut dice = Dice::new(now.elapsed().as_nanos() as u64 ^ 0x51ed);
        let did = match &seen.jigsaw {
            Some(jigsaw) => self.answer(cdp, s, &mut dice, jigsaw, &seen.nodes),
            None => self.tick(cdp, s, &mut dice, &seen),
        };
        let (rest, done) = match did {
            // Nothing to do yet (the jigsaw has not opened, its pictures
            // are still coming): looked at again shortly.
            Ok(None) => {
                self.rests = Some(Instant::now() + AFTER_TICK);
                return None;
            }
            Ok(Some(mut done)) => {
                let ticked = done["step"] == "checkbox";
                if ticked {
                    done["passes_by_itself"] = json!(said);
                    done["after_ms"] = json!(now.duration_since(since).as_millis() as u64);
                }
                (if ticked { AFTER_TICK } else { EVERY }, done)
            }
            // The page did not take the pointer (frozen, or something in
            // the way that would not move): later, and not for ever.
            Err(why) => {
                self.stalls += 1;
                (EVERY, json!({ "step": "stalled", "why": why }))
            }
        };
        let at = Instant::now();
        self.acted = Some(at);
        self.rests = Some(at + rest);
        Some(done)
    }

    /// The checkbox ticked, when its turn has come.
    fn tick(
        &mut self,
        cdp: &mut Cdp,
        s: Option<&str>,
        dice: &mut Dice,
        seen: &Seen,
    ) -> Result<Option<Value>, String> {
        let again = self.acted.is_none_or(|at| at.elapsed() >= TICK_AGAIN);
        let Some(checkbox) = seen.checkbox.filter(|_| self.ticks < TICKS && again) else {
            return Ok(None);
        };
        let (x, y, _, h) = area(cdp, s, checkbox)?;
        press(cdp, s, dice, (x + 30.0, y + h / 2.0), &seen.nodes)?;
        self.ticks += 1;
        Ok(Some(json!({ "step": "checkbox", "try": self.ticks })))
    }

    /// The open jigsaw: its pieces dragged home and Verify pressed, or
    /// another picture asked for.
    fn answer(
        &mut self,
        cdp: &mut Cdp,
        s: Option<&str>,
        dice: &mut Dice,
        jigsaw: &Jigsaw,
        widget: &HashSet<i64>,
    ) -> Result<Option<Value>, String> {
        if jigsaw.pieces.is_empty() || self.verifies >= VERIFIES {
            return Ok(None);
        }
        let read = || -> Option<jigsaw::Puzzle> {
            Some(jigsaw::Puzzle {
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
            })
        };
        // Its pictures are still coming.
        let Some(puzzle) = read() else {
            return Ok(None);
        };
        let began = Instant::now();
        let answer = jigsaw::solve(&puzzle);
        let took = began.elapsed().as_millis() as u64;
        let placed = match answer {
            Ok(jigsaw::Answer::Sure(placed)) => placed,
            Ok(jigsaw::Answer::Unsure(why)) | Err(why) => {
                if self.reloads >= RELOADS {
                    return Ok(None);
                }
                let (x, y, w, h) = area(cdp, s, jigsaw.reload)?;
                press(cdp, s, dice, (x + w / 2.0, y + h / 2.0), widget)?;
                self.reloads += 1;
                return Ok(Some(
                    json!({ "step": "another picture", "why": why, "solver_ms": took }),
                ));
            }
        };
        let (stage_x, stage_y, _, _) = area(cdp, s, jigsaw.stage)?;
        for put in &placed {
            let piece = jigsaw
                .pieces
                .get(put.piece)
                .ok_or("a piece the jigsaw does not have")?;
            let from = (
                stage_x + f64::from(piece.left + piece.width / 2.0),
                stage_y + f64::from(piece.top + piece.height / 2.0),
            );
            let to = (
                from.0 + f64::from(put.left - piece.left),
                from.1 + f64::from(put.top - piece.top),
            );
            drag(cdp, s, dice, from, to, widget)?;
        }
        let (x, y, w, h) = area(cdp, s, jigsaw.verify)?;
        press(cdp, s, dice, (x + w / 2.0, y + h / 2.0), widget)?;
        self.verifies += 1;
        Ok(Some(json!({
            "step": "jigsaw",
            "try": self.verifies,
            "pieces": placed.len(),
            "fit": placed.iter().map(|p| (p.fit * 100.0).round() / 100.0).collect::<Vec<_>>(),
            "solver_ms": took,
        })))
    }
}

/// Where node `node` is drawn on the page.
fn area(cdp: &mut Cdp, s: Option<&str>, node: i64) -> Result<Area, String> {
    let model = cdp.call("DOM.getBoxModel", json!({ "nodeId": node }), s)?;
    let q: Vec<f64> = model["model"]["border"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    if q.len() == 8 {
        Ok((q[0], q[1], q[2] - q[0], q[5] - q[1]))
    } else {
        Err("a part of the widget is not drawn".into())
    }
}

/// Makes sure a click at `x`, `y` lands on the widget: whatever lies over
/// it there (an advert's frame or layer) is made to let clicks through,
/// and with something still in the way nothing is clicked.
fn clear_way(
    cdp: &mut Cdp,
    s: Option<&str>,
    (x, y): (f64, f64),
    widget: &HashSet<i64>,
) -> Result<(), String> {
    // DevTools is asked what lies at a place in the document, and the
    // widget's parts are measured in the window: on a page scrolled down
    // the two differ by how far.
    let metrics = cdp.call("Page.getLayoutMetrics", json!({}), s)?;
    let scrolled = |axis: &str| metrics["cssVisualViewport"][axis].as_f64().unwrap_or(0.0);
    let (in_x, in_y) = (
        (x + scrolled("pageX")).round() as i64,
        (y + scrolled("pageY")).round() as i64,
    );
    for _ in 0..8 {
        let at = cdp.call(
            "DOM.getNodeForLocation",
            json!({ "x": in_x, "y": in_y, "includeUserAgentShadowDOM": true }),
            s,
        )?;
        if at["backendNodeId"]
            .as_i64()
            .is_some_and(|n| widget.contains(&n))
        {
            return Ok(());
        }
        let script = UNCOVER
            .replace('X', &x.to_string())
            .replace('Y', &y.to_string());
        let moved = cdp.call(
            "Runtime.evaluate",
            json!({ "expression": script, "returnByValue": true }),
            s,
        )?;
        if moved["result"]["value"].as_bool() != Some(true) {
            break;
        }
    }
    Err("something lies over the widget".into())
}

fn mouse(
    cdp: &mut Cdp,
    s: Option<&str>,
    kind: &str,
    (x, y): (f64, f64),
    held: bool,
) -> Result<(), String> {
    let pressing = kind != "mouseMoved";
    cdp.call(
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
    )
    .map(|_| ())
}

fn rest(dice: &mut Dice, from_ms: f32, to_ms: f32) {
    std::thread::sleep(Duration::from_millis(dice.between(from_ms, to_ms) as u64));
}

/// The pointer comes to `at` from a little way off, and clicks, once the
/// way to the widget there is clear.
fn press(
    cdp: &mut Cdp,
    s: Option<&str>,
    dice: &mut Dice,
    at: (f64, f64),
    widget: &HashSet<i64>,
) -> Result<(), String> {
    clear_way(cdp, s, at, widget)?;
    let from = (
        at.0 + f64::from(dice.between(40.0, 140.0)),
        at.1 + f64::from(dice.between(20.0, 70.0)),
    );
    glide(cdp, s, dice, from, at, false)?;
    rest(dice, 80.0, 180.0);
    mouse(cdp, s, "mousePressed", at, true)?;
    rest(dice, 60.0, 120.0);
    mouse(cdp, s, "mouseReleased", at, false)
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
) -> Result<(), String> {
    let steps = dice.between(16.0, 26.0) as u32;
    let bow = f64::from(dice.between(-16.0, 16.0));
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        let eased = t * t * (3.0 - 2.0 * t);
        let arc = (std::f64::consts::PI * t).sin() * bow;
        let mut at = (
            from.0 + (to.0 - from.0) * eased + arc * 0.3 + f64::from(dice.between(-0.6, 0.6)),
            from.1 + (to.1 - from.1) * eased + arc + f64::from(dice.between(-0.6, 0.6)),
        );
        if step == steps {
            at = to;
        }
        mouse(cdp, s, "mouseMoved", at, held)?;
        rest(dice, 10.0, 30.0);
    }
    Ok(())
}

/// A piece taken at `from` and let go at `to`, once the way to it is
/// clear.
fn drag(
    cdp: &mut Cdp,
    s: Option<&str>,
    dice: &mut Dice,
    from: (f64, f64),
    to: (f64, f64),
    widget: &HashSet<i64>,
) -> Result<(), String> {
    clear_way(cdp, s, from, widget)?;
    let near = (
        from.0 - f64::from(dice.between(20.0, 60.0)),
        from.1 + f64::from(dice.between(15.0, 40.0)),
    );
    glide(cdp, s, dice, near, from, false)?;
    rest(dice, 90.0, 200.0);
    mouse(cdp, s, "mousePressed", from, true)?;
    rest(dice, 80.0, 160.0);
    glide(cdp, s, dice, from, to, true)?;
    rest(dice, 120.0, 260.0);
    mouse(cdp, s, "mouseReleased", to, false)?;
    rest(dice, 250.0, 500.0);
    Ok(())
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
        assert!(
            seen.nodes.is_empty(),
            "no lasting numbers in this made-up document"
        );
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

    /// The service's word on a visit is read from the answer to the
    /// widget's question, and from nothing else the page loads.
    #[test]
    fn the_services_word_is_heard_on_the_network() {
        let got = |id: &str, kind: &str, url: &str| json!({ "method": "Network.responseReceived", "params": { "requestId": id, "type": kind, "response": { "url": url } } });
        let done = |id: &str| json!({ "method": "Network.loadingFinished", "params": { "requestId": id } });
        let body = |id: u64, body: &str| json!({ "id": id, "result": { "body": body, "base64Encoded": false } });
        let ask = "https://api.cutcaptcha.net/v1/challenge";
        let mut heard = Heard::default();
        // Another answer of the service's, the browser's question before
        // the widget's, and another site's page are not it.
        assert_eq!(
            heard.finished(&got("1", "Fetch", "https://api.cutcaptcha.net/v1/session")),
            None
        );
        assert_eq!(heard.finished(&done("1")), None);
        assert_eq!(heard.finished(&got("2", "Preflight", ask)), None);
        assert_eq!(heard.finished(&done("2")), None);
        assert_eq!(
            heard.finished(&got("3", "Fetch", "https://site.example/v1/challenge")),
            None
        );
        assert_eq!(heard.finished(&done("3")), None);
        // The widget's question: its body is asked for once it has come.
        assert_eq!(heard.finished(&got("4", "Fetch", ask)), None);
        assert_eq!(heard.finished(&done("4")), Some("4".to_string()));
        heard.asked(70);
        // Another question's answer says nothing.
        heard.answer(&body(69, r#"{"data":{"auto":true}}"#));
        assert_eq!(heard.by_itself, None);
        heard.answer(&body(
            70,
            r#"{"data":{"auto":false,"pow":{"d":19}},"success":true}"#,
        ));
        assert_eq!(heard.by_itself, Some(false));
        // The next visit's answer takes its place, and an answer that says
        // nothing of it leaves the widget to the clock.
        assert_eq!(heard.finished(&got("5", "Fetch", ask)), None);
        assert_eq!(heard.by_itself, None);
        assert_eq!(heard.finished(&done("5")), Some("5".to_string()));
        heard.asked(71);
        heard.answer(&body(71, r#"{"data":{"auto":true}}"#));
        assert_eq!(heard.by_itself, Some(true));
        assert_eq!(heard.finished(&got("6", "Fetch", ask)), None);
        assert_eq!(heard.finished(&done("6")), Some("6".to_string()));
        heard.asked(72);
        heard.answer(&body(72, r#"{"success":false}"#));
        assert_eq!(heard.by_itself, None);
    }
}
