//! Solving the jigsaw a CutCaptcha widget shows a visitor it doubts: a
//! picture with holes cut in it and a few pieces to drag into the holes
//! they came from, with more holes than pieces, and two holes of every
//! piece's shape. The shape tells which holes a piece fits; the picture
//! around a hole tells which of them it came from.
//!
//! The widget hands the page its picture and pieces as images, so this
//! works on those alone, on this computer: no service is asked. It says
//! where each piece goes only when it is sure, since a wrong answer costs
//! more than asking the widget for another picture.
//!
//! - A hole is the picture under it darkened flat: dark, and even over a
//!   few pixels, whatever tint it takes from what it covers.
//! - A piece fits a hole when their outlines overlap, centre on centre.
//! - Of the holes it fits, it came from the one whose surroundings carry
//!   on the colours along the piece's rim.

use image::{RgbImage, RgbaImage};

/// The puzzle as the widget shows it: images as it holds them (encoded),
/// and the sizes it draws them at.
pub struct Puzzle {
    pub picture: Vec<u8>,
    /// The picture's drawn width, in the page's pixels.
    pub width: f32,
    pub pieces: Vec<Piece>,
}

pub struct Piece {
    pub image: Vec<u8>,
    /// The piece's image's drawn width, in the page's pixels.
    pub width: f32,
}

/// Where piece `piece`'s image goes, its top left corner from the
/// picture's, in the page's pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub piece: usize,
    pub left: f32,
    pub top: f32,
    /// How well its outline overlaps its hole's, 0 to 1.
    pub fit: f32,
    /// How far ahead of the piece's next best hole this one is.
    pub lead: f32,
}

#[derive(Debug, PartialEq)]
pub enum Answer {
    /// Every piece has a hole it clearly came from.
    Sure(Vec<Placed>),
    /// Not clear enough to answer: why.
    Unsure(String),
}

/// A fit below this is no fit: another shape's hole.
const FITS: f32 = 0.6;
/// An answer needs every piece to fit this well, and lead by this much.
const SURE_FIT: f32 = 0.7;
const SURE_LEAD: f32 = 8.0;
/// A hole's flat core is this much smaller than the hole, all round.
const GROW: usize = 7;

/// The bytes a `data:` address holds, when it is base64.
pub fn data_uri(uri: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let (head, data) = uri.split_once(',')?;
    (head.starts_with("data:") && head.ends_with(";base64"))
        .then(|| base64::engine::general_purpose::STANDARD.decode(data).ok())?
}

/// Where the puzzle's pieces go, or why that is not clear.
pub fn solve(puzzle: &Puzzle) -> Result<Answer, String> {
    let picture = image::load_from_memory(&puzzle.picture)
        .map_err(|e| format!("the puzzle's picture: {e}"))?
        .to_rgb8();
    let mut pieces = Vec::new();
    for p in &puzzle.pieces {
        let image = image::load_from_memory(&p.image)
            .map_err(|e| format!("a puzzle piece: {e}"))?
            .to_rgba8();
        pieces.push((image, p.width));
    }
    Ok(solve_images(&picture, &pieces))
}

/// A grid of yes and no.
#[derive(Clone)]
struct Mask {
    w: usize,
    h: usize,
    on: Vec<bool>,
}

impl Mask {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            on: vec![false; w * h],
        }
    }

    fn at(&self, x: isize, y: isize) -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < self.w
            && (y as usize) < self.h
            && self.on[y as usize * self.w + x as usize]
    }

    fn count(&self) -> usize {
        self.on.iter().filter(|&&b| b).count()
    }

    /// One step in or out: a cell and its four neighbours, with nothing
    /// beyond the grid's edge.
    fn step(&self, grow: bool) -> Self {
        let (w, h) = (self.w, self.h);
        let mut out = Self::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let cells = [
                    self.on[i],
                    x > 0 && self.on[i - 1],
                    x + 1 < w && self.on[i + 1],
                    y > 0 && self.on[i - w],
                    y + 1 < h && self.on[i + w],
                ];
                out.on[i] = if grow {
                    cells.contains(&true)
                } else {
                    !cells.contains(&false)
                };
            }
        }
        out
    }

    fn grown(&self, steps: usize) -> Self {
        (0..steps).fold(self.clone(), |m, _| m.step(true))
    }

    fn shrunk(&self, steps: usize) -> Self {
        (0..steps).fold(self.clone(), |m, _| m.step(false))
    }

    /// The smallest box holding every yes: left, top, right, bottom.
    fn bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.on[y * self.w + x] {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        (x1 > 0).then_some((x0, y0, x1, y1))
    }
}

/// Sums of `values` over the square of `radius` around every cell (a
/// summed-area table), cut off at the grid's edge.
fn box_sums(values: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut table = vec![0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += f64::from(values[y * w + x]);
            table[(y + 1) * (w + 1) + x + 1] = table[y * (w + 1) + x + 1] + row;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(radius), (y + radius + 1).min(h));
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(radius), (x + radius + 1).min(w));
            let sum =
                table[y1 * (w + 1) + x1] - table[y0 * (w + 1) + x1] - table[y1 * (w + 1) + x0]
                    + table[y0 * (w + 1) + x0];
            out[y * w + x] = sum as f32;
        }
    }
    out
}

/// A hole in the picture: its shape on a grid of its own, placed at `x`,
/// `y` in the picture, and the box of the shape there.
struct Hole {
    x: isize,
    y: isize,
    shape: Mask,
    bounds: (isize, isize, isize, isize),
}

impl Hole {
    fn at(&self, x: isize, y: isize) -> bool {
        self.shape.at(x - self.x, y - self.y)
    }
}

/// A piece: its shape and colours, cut to the shape's box, the box's
/// corner in the piece's image, and the image's pixels per page pixel.
struct Shape {
    mask: Mask,
    /// A band a little inside its edge, past its outline: the colours a
    /// hole's surroundings are held against.
    rim: Mask,
    rgb: Vec<[f32; 3]>,
    corner: (usize, usize),
    scale: f32,
}

/// The flat dark cells of the picture, and the holes among them of about
/// a piece's size.
fn holes(picture: &RgbImage, least: usize, most: usize) -> (Mask, Vec<Hole>) {
    let (w, h) = (picture.width() as usize, picture.height() as usize);
    let grey: Vec<f32> = picture
        .pixels()
        .map(|p| (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0)
        .collect();
    let squares: Vec<f32> = grey.iter().map(|g| g * g).collect();
    let (sum, sum_sq) = (box_sums(&grey, w, h, 2), box_sums(&squares, w, h, 2));
    let ones = box_sums(&vec![1.0; w * h], w, h, 2);
    let mut dark = Mask::new(w, h);
    for (i, p) in picture.pixels().enumerate() {
        let mean = sum[i] / ones[i];
        let var = (sum_sq[i] / ones[i] - mean * mean).max(0.0);
        dark.on[i] = p[0].max(p[1]).max(p[2]) < 112 && var < 36.0;
    }
    let dark = dark.shrunk(3).grown(3);
    // Each island of dark cells, by flooding from a cell not yet seen.
    let mut seen = vec![false; w * h];
    let mut found = Vec::new();
    for start in 0..w * h {
        if !dark.on[start] || seen[start] {
            continue;
        }
        let mut cells = vec![start];
        seen[start] = true;
        let mut next = 0;
        while next < cells.len() {
            let (x, y) = (cells[next] % w, cells[next] / w);
            next += 1;
            let around = [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ];
            for (nx, ny) in around {
                if nx < w && ny < h && dark.on[ny * w + nx] && !seen[ny * w + nx] {
                    seen[ny * w + nx] = true;
                    cells.push(ny * w + nx);
                }
            }
        }
        if cells.len() < least || cells.len() > most {
            continue;
        }
        // On a grid of its own with room to grow: filled, then grown back
        // to the hole's edge.
        let pad = GROW + 4;
        let (x0, y0) = (
            cells.iter().map(|c| c % w).min().unwrap_or(0),
            cells.iter().map(|c| c / w).min().unwrap_or(0),
        );
        let (x1, y1) = (
            cells.iter().map(|c| c % w).max().unwrap_or(0) + 1,
            cells.iter().map(|c| c / w).max().unwrap_or(0) + 1,
        );
        let mut shape = Mask::new(x1 - x0 + 2 * pad, y1 - y0 + 2 * pad);
        for c in &cells {
            shape.on[(c / w - y0 + pad) * shape.w + (c % w - x0 + pad)] = true;
        }
        let shape = filled(&shape).grown(GROW);
        let Some(b) = shape.bounds() else {
            continue;
        };
        let (x, y) = (x0 as isize - pad as isize, y0 as isize - pad as isize);
        found.push(Hole {
            x,
            y,
            bounds: (
                x + b.0 as isize,
                y + b.1 as isize,
                x + b.2 as isize,
                y + b.3 as isize,
            ),
            shape,
        });
    }
    (dark, found)
}

/// `shape` with the cells it encloses: all that a flood from the grid's
/// edge does not reach.
fn filled(shape: &Mask) -> Mask {
    let (w, h) = (shape.w, shape.h);
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for x in 0..w {
        stack.extend([x, (h - 1) * w + x]);
    }
    for y in 0..h {
        stack.extend([y * w, y * w + w - 1]);
    }
    while let Some(c) = stack.pop() {
        if outside[c] || shape.on[c] {
            continue;
        }
        outside[c] = true;
        let (x, y) = (c % w, c / w);
        if x > 0 {
            stack.push(c - 1);
        }
        if x + 1 < w {
            stack.push(c + 1);
        }
        if y > 0 {
            stack.push(c - w);
        }
        if y + 1 < h {
            stack.push(c + w);
        }
    }
    Mask {
        w,
        h,
        on: outside.iter().map(|o| !o).collect(),
    }
}

/// The piece in `image`, drawn `width` page pixels wide.
fn shape_of(image: &RgbaImage, width: f32) -> Option<Shape> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut whole = Mask::new(w, h);
    for (i, p) in image.pixels().enumerate() {
        whole.on[i] = p[3] > 128;
    }
    let (x0, y0, x1, y1) = whole.bounds()?;
    let mut mask = Mask::new(x1 - x0, y1 - y0);
    let mut rgb = Vec::with_capacity(mask.w * mask.h);
    for y in y0..y1 {
        for x in x0..x1 {
            mask.on[(y - y0) * mask.w + (x - x0)] = whole.on[y * w + x];
            let p = image.get_pixel(x as u32, y as u32);
            rgb.push([f32::from(p[0]), f32::from(p[1]), f32::from(p[2])]);
        }
    }
    let inner = mask.shrunk(5);
    let core = inner.shrunk(7);
    let mut rim = inner;
    for (cell, in_core) in rim.on.iter_mut().zip(&core.on) {
        *cell = *cell && !in_core;
    }
    Some(Shape {
        mask,
        rim,
        rgb,
        corner: (x0, y0),
        scale: w as f32 / width.max(1.0),
    })
}

/// How well `piece` fits `hole`, centre on centre and a few cells around:
/// the overlap of the two shapes over their union, and where the piece's
/// box then sits in the picture.
fn fit(piece: &Shape, hole: &Hole, picture: (usize, usize)) -> (f32, isize, isize) {
    let (pw, ph) = (piece.mask.w as isize, piece.mask.h as isize);
    let (hx0, hy0, hx1, hy1) = hole.bounds;
    let (cx, cy) = ((hx0 + hx1) / 2 - pw / 2, (hy0 + hy1) / 2 - ph / 2);
    let mut best = (-1.0, cx, cy);
    for dy in (-6..=6).step_by(2) {
        for dx in (-6..=6).step_by(2) {
            let (ox, oy) = (cx + dx, cy + dy);
            if ox < 0 || oy < 0 || ox + pw > picture.0 as isize || oy + ph > picture.1 as isize {
                continue;
            }
            let (mut both, mut either) = (0u32, 0u32);
            for y in 0..ph {
                for x in 0..pw {
                    let p = piece.mask.on[(y * pw + x) as usize];
                    let h = hole.at(ox + x, oy + y);
                    both += u32::from(p && h);
                    either += u32::from(p || h);
                }
            }
            let overlap = both as f32 / either.max(1) as f32;
            if overlap > best.0 {
                best = (overlap, ox, oy);
            }
        }
    }
    best
}

/// How far the picture just outside the hole is from the colours along the
/// piece's rim, with the piece's box at `ox`, `oy`: the mean difference
/// per colour channel, 0 to 255, lower for the hole it came from.
fn picture_gap(piece: &Shape, picture: &RgbImage, dark: &Mask, ox: isize, oy: isize) -> f32 {
    const PAD: isize = 24;
    let (pw, ph) = (piece.mask.w as isize, piece.mask.h as isize);
    let (w, h) = ((pw + 2 * PAD) as usize, (ph + 2 * PAD) as usize);
    let mut placed = Mask::new(w, h);
    let mut colour = [vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]];
    let mut weight = vec![0f32; w * h];
    for y in 0..ph {
        for x in 0..pw {
            let (i, at) = (
                (y * pw + x) as usize,
                ((y + PAD) * w as isize + x + PAD) as usize,
            );
            placed.on[at] = piece.mask.on[i];
            if piece.rim.on[i] {
                for (c, channel) in colour.iter_mut().enumerate() {
                    channel[at] = piece.rgb[i][c];
                }
                weight[at] = 1.0;
            }
        }
    }
    // The picture's band a little outside the hole, past its outline.
    let near = placed.grown(5);
    let far = near.grown(7);
    let sums: Vec<Vec<f32>> = colour.iter().map(|c| box_sums(c, w, h, 10)).collect();
    let weights = box_sums(&weight, w, h, 10);
    let (mut gap, mut cells) = (0f32, 0u32);
    for y in 0..h as isize {
        for x in 0..w as isize {
            let at = (y * w as isize + x) as usize;
            let (px, py) = (ox + x - PAD, oy + y - PAD);
            let inside = px >= 0
                && py >= 0
                && px < picture.width() as isize
                && py < picture.height() as isize;
            if !far.on[at] || near.on[at] || !inside || dark.at(px, py) || weights[at] < 9.0 {
                continue;
            }
            let p = picture.get_pixel(px as u32, py as u32);
            for (c, sum) in sums.iter().enumerate() {
                gap += (sum[at] / weights[at] - f32::from(p[c])).abs();
            }
            cells += 1;
        }
    }
    if cells > 50 {
        gap / (cells * 3) as f32
    } else {
        255.0
    }
}

/// [`solve`] on images already read.
pub fn solve_images(picture: &RgbImage, pieces: &[(RgbaImage, f32)]) -> Answer {
    let shapes: Vec<Shape> = pieces
        .iter()
        .filter_map(|(image, width)| shape_of(image, *width))
        .collect();
    if shapes.is_empty() || shapes.len() != pieces.len() {
        return Answer::Unsure("a piece with nothing in it".into());
    }
    let areas: Vec<usize> = shapes.iter().map(|s| s.mask.count()).collect();
    let least = areas.iter().min().copied().unwrap_or(0) * 45 / 100;
    let most = areas.iter().max().copied().unwrap_or(0) * 140 / 100;
    let (dark, holes) = holes(picture, least, most);
    if holes.len() < shapes.len() {
        return Answer::Unsure(format!(
            "{} holes found for {} pieces",
            holes.len(),
            shapes.len()
        ));
    }
    let size = (picture.width() as usize, picture.height() as usize);
    // Each piece in each hole: how well, and where.
    let mut scores = vec![vec![(-1000f32, 0f32, 0isize, 0isize); holes.len()]; shapes.len()];
    for (i, piece) in shapes.iter().enumerate() {
        for (j, hole) in holes.iter().enumerate() {
            let (overlap, ox, oy) = fit(piece, hole, size);
            if overlap > FITS {
                let gap = picture_gap(piece, picture, &dark, ox, oy);
                scores[i][j] = (overlap * 100.0 - gap * 0.6, overlap, ox, oy);
            }
        }
    }
    // The best way to give every piece a hole of its own.
    fn best(
        scores: &[Vec<(f32, f32, isize, isize)>],
        piece: usize,
        taken: &mut Vec<usize>,
        top: &mut (f32, Vec<usize>),
        sum: f32,
    ) {
        if piece == scores.len() {
            if sum > top.0 {
                *top = (sum, taken.clone());
            }
            return;
        }
        for hole in 0..scores[piece].len() {
            if !taken.contains(&hole) {
                taken.push(hole);
                best(scores, piece + 1, taken, top, sum + scores[piece][hole].0);
                taken.pop();
            }
        }
    }
    let mut top = (f32::MIN, Vec::new());
    best(&scores, 0, &mut Vec::new(), &mut top, 0.0);
    let mut placed = Vec::new();
    for (i, &j) in top.1.iter().enumerate() {
        let (score, overlap, ox, oy) = scores[i][j];
        let next = scores[i]
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != j)
            .map(|(_, s)| s.0)
            .fold(-1000f32, f32::max);
        let lead = score - next;
        if overlap < SURE_FIT || lead < SURE_LEAD {
            return Answer::Unsure(format!(
                "piece {} fits {overlap:.2} and leads by {lead:.0}",
                i + 1
            ));
        }
        let shape = &shapes[i];
        placed.push(Placed {
            piece: i,
            left: (ox as f32 - shape.corner.0 as f32) / shape.scale,
            top: (oy as f32 - shape.corner.1 as f32) / shape.scale,
            fit: overlap,
            lead,
        });
    }
    Answer::Sure(placed)
}

/// Made-up puzzles of the widget's kind, for the tests and for a page that
/// stands in for the widget: a picture of coloured patches, jigsaw shapes
/// cut out of it, and two holes of each shape.
pub mod made_up {
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    /// A made-up puzzle and its answer.
    pub struct MadeUp {
        pub picture: RgbImage,
        /// Each piece's image and the width it is drawn at.
        pub pieces: Vec<(RgbaImage, f32)>,
        /// Where each piece's image goes, in page pixels.
        pub answer: Vec<(f32, f32)>,
    }

    fn png(image: &image::DynamicImage) -> String {
        use base64::Engine as _;
        let mut png = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("an image in memory is written");
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    impl MadeUp {
        /// The picture as a `data:` address, as the widget hands it over.
        pub fn picture_uri(&self) -> String {
            png(&image::DynamicImage::ImageRgb8(self.picture.clone()))
        }

        /// Each piece's image as a `data:` address, the width it is drawn
        /// at, and where it goes.
        pub fn piece_uris(&self) -> Vec<(String, f32, (f32, f32))> {
            self.pieces
                .iter()
                .zip(&self.answer)
                .map(|((image, width), answer)| {
                    (
                        png(&image::DynamicImage::ImageRgba8(image.clone())),
                        *width,
                        *answer,
                    )
                })
                .collect()
        }
    }

    /// Numbers that look random and repeat for a seed.
    pub struct Dice(u64);

    impl Dice {
        pub fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        /// From 0 up to 1.
        pub fn roll(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }

        pub fn between(&mut self, low: f32, high: f32) -> f32 {
            low + self.roll() * (high - low)
        }
    }

    /// A jigsaw piece's outline: a square with a round tab or notch on each
    /// side, turned a little.
    struct Outline {
        half: f32,
        turn: f32,
        /// Per side: out (1) or in (-1), and where along the side.
        sides: [(f32, f32); 4],
    }

    impl Outline {
        fn holds(&self, x: f32, y: f32) -> bool {
            let (s, c) = self.turn.sin_cos();
            let (x, y) = (x * c + y * s, -x * s + y * c);
            let knob = self.half * 0.36;
            let centres = [
                (self.sides[0].1, -self.half),
                (self.half, self.sides[1].1),
                (self.sides[2].1, self.half),
                (-self.half, self.sides[3].1),
            ];
            let mut inside = x.abs() <= self.half && y.abs() <= self.half;
            for (side, (cx, cy)) in self.sides.iter().zip(centres) {
                let near = (x - cx).hypot(y - cy) <= knob;
                if near {
                    inside = side.0 > 0.0;
                }
            }
            inside
        }
    }

    /// Puzzle `seed`: `pieces` pieces, and as many more holes of their
    /// shapes that stay empty, on a picture 960 by 640 drawn at half that.
    pub fn puzzle(seed: u64, pieces: usize) -> MadeUp {
        let mut dice = Dice::new(seed);
        let (w, h) = (960u32, 640u32);
        // Bright patches and stripes, so no two places look alike.
        let patches: Vec<(f32, f32, f32, [f32; 3])> = (0..70)
            .map(|_| {
                (
                    dice.between(0.0, w as f32),
                    dice.between(0.0, h as f32),
                    dice.between(25.0, 110.0),
                    [
                        dice.between(60.0, 255.0),
                        dice.between(60.0, 255.0),
                        dice.between(60.0, 255.0),
                    ],
                )
            })
            .collect();
        let stripes = (dice.between(0.02, 0.07), dice.between(0.02, 0.07));
        let colour = |x: f32, y: f32| -> [f32; 3] {
            let mut c = [
                150.0 + 60.0 * (x * stripes.0).sin(),
                160.0,
                150.0 + 60.0 * (y * stripes.1).cos(),
            ];
            for (px, py, r, tint) in &patches {
                if (x - px).hypot(y - py) < *r {
                    c = *tint;
                }
            }
            c
        };
        let mut picture = RgbImage::from_fn(w, h, |x, y| {
            let c = colour(x as f32, y as f32);
            Rgb([c[0] as u8, c[1] as u8, c[2] as u8])
        });
        let whole = picture.clone();
        // Places for the holes, apart from each other and from the edge.
        let mut spots: Vec<(f32, f32)> = Vec::new();
        while spots.len() < pieces * 2 {
            let spot = (
                dice.between(110.0, w as f32 - 110.0),
                dice.between(110.0, h as f32 - 110.0),
            );
            if spots
                .iter()
                .all(|s| (s.0 - spot.0).hypot(s.1 - spot.1) > 190.0)
            {
                spots.push(spot);
            }
        }
        let mut made = MadeUp {
            picture: RgbImage::new(1, 1),
            pieces: Vec::new(),
            answer: Vec::new(),
        };
        for piece in 0..pieces {
            let outline = Outline {
                half: dice.between(52.0, 62.0),
                turn: dice.between(-0.3, 0.3),
                sides: [(); 4].map(|()| {
                    (
                        if dice.roll() < 0.5 { 1.0 } else { -1.0 },
                        dice.between(-14.0, 14.0),
                    )
                }),
            };
            let reach = (outline.half * 1.9) as i32;
            // Its own hole, then the one of its shape that stays empty.
            for (which, spot) in [spots[piece * 2], spots[piece * 2 + 1]]
                .into_iter()
                .enumerate()
            {
                let (mut sum, mut cells) = ([0f32; 3], 0f32);
                for dy in -reach..=reach {
                    for dx in -reach..=reach {
                        if outline.holds(dx as f32, dy as f32) {
                            let p = whole.get_pixel(
                                (spot.0 as i32 + dx) as u32,
                                (spot.1 as i32 + dy) as u32,
                            );
                            for c in 0..3 {
                                sum[c] += f32::from(p[c]);
                            }
                            cells += 1.0;
                        }
                    }
                }
                for dy in -reach..=reach {
                    for dx in -reach..=reach {
                        let (x, y) = ((spot.0 as i32 + dx) as u32, (spot.1 as i32 + dy) as u32);
                        let inside = outline.holds(dx as f32, dy as f32);
                        let edge = !inside
                            && [(2, 0), (-2, 0), (0, 2), (0, -2)]
                                .iter()
                                .any(|(ex, ey)| outline.holds((dx + ex) as f32, (dy + ey) as f32));
                        if inside {
                            let grain = dice.between(-2.0, 2.0);
                            picture.put_pixel(
                                x,
                                y,
                                Rgb([0, 1, 2].map(|c| (sum[c] / cells * 0.27 + 8.0 + grain) as u8)),
                            );
                        } else if edge {
                            picture.put_pixel(x, y, Rgb([245, 245, 245]));
                        }
                    }
                }
                if which == 0 {
                    let side = (reach * 2 + 1) as u32;
                    let image = RgbaImage::from_fn(side, side, |ix, iy| {
                        let (dx, dy) = (ix as i32 - reach, iy as i32 - reach);
                        let inside = outline.holds(dx as f32, dy as f32);
                        let edge = !inside
                            && [(2, 0), (-2, 0), (0, 2), (0, -2)]
                                .iter()
                                .any(|(ex, ey)| outline.holds((dx + ex) as f32, (dy + ey) as f32));
                        if inside {
                            let p = whole.get_pixel(
                                (spot.0 as i32 + dx) as u32,
                                (spot.1 as i32 + dy) as u32,
                            );
                            Rgba([p[0], p[1], p[2], 255])
                        } else if edge {
                            Rgba([245, 245, 245, 255])
                        } else {
                            Rgba([0, 0, 0, 0])
                        }
                    });
                    made.answer
                        .push(((spot.0 - reach as f32) / 2.0, (spot.1 - reach as f32) / 2.0));
                    made.pieces.push((image, side as f32 / 2.0));
                }
            }
        }
        made.picture = picture;
        made
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Made-up puzzles of three pieces and six holes: an answer given is
    /// right to the pixel or two, and most puzzles get one.
    #[test]
    fn made_up_puzzles_are_solved_or_left() {
        let (mut sure, mut wrong) = (0, Vec::new());
        let puzzles = 10;
        for seed in 1..=puzzles {
            let made = made_up::puzzle(seed, 3);
            match solve_images(&made.picture, &made.pieces) {
                Answer::Sure(placed) => {
                    sure += 1;
                    for p in &placed {
                        let (left, top) = made.answer[p.piece];
                        if (p.left - left).abs() > 3.0 || (p.top - top).abs() > 3.0 {
                            wrong.push(format!(
                                "puzzle {seed}, piece {}: {:.1},{:.1} for {left:.1},{top:.1}",
                                p.piece, p.left, p.top
                            ));
                        }
                    }
                }
                Answer::Unsure(why) => println!("puzzle {seed}: unsure, {why}"),
            }
        }
        println!("{sure} of {puzzles} made-up puzzles answered");
        assert!(wrong.is_empty(), "sure and wrong:\n{}", wrong.join("\n"));
        assert!(
            sure * 10 >= puzzles * 8,
            "only {sure} of {puzzles} answered"
        );
    }

    /// For puzzles collected from the real widget and kept outside the
    /// repository: `HAUL_JIGSAW_SAMPLES` names a folder of `name.jpg`
    /// pictures with their pieces `namea.png`, `nameb.png`…, and each
    /// one's answer is printed. Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "needs a folder of puzzles"]
    fn puzzles_in_a_folder_are_answered() {
        let folder = std::env::var("HAUL_JIGSAW_SAMPLES").expect("HAUL_JIGSAW_SAMPLES");
        let mut pictures: Vec<_> = std::fs::read_dir(&folder)
            .expect("the folder")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
            .collect();
        pictures.sort();
        for picture in pictures {
            let name = picture.file_stem().unwrap().to_string_lossy().to_string();
            let pieces: Vec<Piece> = ('a'..='e')
                .filter_map(|letter| {
                    std::fs::read(picture.with_file_name(format!("{name}{letter}.png"))).ok()
                })
                .map(|image| {
                    let width =
                        image::load_from_memory(&image).expect("a piece").width() as f32 / 2.0;
                    Piece { image, width }
                })
                .collect();
            let puzzle = Puzzle {
                picture: std::fs::read(&picture).expect("the picture"),
                width: 480.0,
                pieces,
            };
            let began = std::time::Instant::now();
            println!("{name}: {:?} in {:?}", solve(&puzzle), began.elapsed());
        }
    }

    #[test]
    fn a_data_address_gives_its_bytes() {
        assert_eq!(data_uri("data:image/png;base64,AAEC"), Some(vec![0, 1, 2]));
        assert_eq!(data_uri("https://site.example/a.png"), None);
    }

    #[test]
    fn a_picture_without_holes_is_left() {
        let made = made_up::puzzle(1, 2);
        let blank = RgbImage::from_pixel(960, 640, image::Rgb([200, 180, 160]));
        assert!(matches!(
            solve_images(&blank, &made.pieces),
            Answer::Unsure(_)
        ));
    }
}
