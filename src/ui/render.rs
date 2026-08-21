//! UI-native replacements for the `sdl2::rect`/`sdl2::pixels` types that used to
//! leak into `ui::DrawCmd`. `platform::webos::compositor` converts to/from SDL at
//! the boundary; `ui`/`app` only ever see these.

/// Integer rectangle. API mirrors the subset of `sdl2::rect::Rect` this crate used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn x(&self) -> i32 {
        self.x
    }

    pub fn y(&self) -> i32 {
        self.y
    }

    pub fn width(&self) -> u32 {
        self.w
    }

    pub fn height(&self) -> u32 {
        self.h
    }

    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    pub fn offset(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Grown by `pad` on every side. The padded region a tile is rasterized into (its
    /// shadow and focus ring live in that margin), and the same rect the compositor has to
    /// draw it back to.
    pub fn inflate(self, pad: i32) -> Self {
        Self::new(
            self.x - pad,
            self.y - pad,
            (self.w as i32 + 2 * pad).max(0) as u32,
            (self.h as i32 + 2 * pad).max(0) as u32,
        )
    }

    /// Inset by `pad` on the left and right, full height. The content column inside a
    /// card: one pad governs both edges, so there is nothing here for a [`Layout`] split
    /// to keep in agreement.
    ///
    /// [`Layout`]: crate::ui::layout::Layout
    pub fn inset_x(self, pad: u32) -> Self {
        Self::new(self.x + pad as i32, self.y, self.w.saturating_sub(2 * pad), self.h)
    }

    pub fn contains_point(&self, p: (i32, i32)) -> bool {
        let (px, py) = p;
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// Overlap of `self` and `other`, or `None` if they don't intersect (matches
    /// `sdl2::rect::Rect::intersection`).
    pub fn intersection(&self, other: Self) -> Option<Self> {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = self.right().min(other.right());
        let y2 = self.bottom().min(other.bottom());
        if x2 <= x1 || y2 <= y1 {
            return None;
        }
        Some(Self::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32))
    }
}

/// Float rectangle, for the one case where whole-pixel placement is too coarse: a pan
/// slow enough that an integer destination would advance in visible jumps rather than
/// drift (see `DrawCmd::TexF`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Straight-alpha RGBA8. Mirrors `sdl2::pixels::Color`'s public field layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[allow(non_snake_case)]
impl Color {
    pub const fn RGBA(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn RGB(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

/// One cached tile's identity: an opaque number the *app* assigns.
///
/// It used to be an enum naming this app's screens (`NoHost`, `PinBadge`, `Hero(String)`),
/// which made `ui` unusable by anything else and put a `String` clone plus a hash of that
/// string on every draw command of every frame. The library only ever needs to tell two
/// tiles apart, so a `Copy` integer is the whole requirement; which number means what is
/// `app::render::tile`'s business.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TileId(pub u32);

/// One step of a frame's composition, in paint order.
pub enum DrawCmd {
    Tex {
        tile: TileId,
        dst: Rect,
        alpha: u8,
    },
    TexCropped {
        tile: TileId,
        src: Rect,
        dst: Rect,
        alpha: u8,
    },
    /// Whole texture to a subpixel destination, clipped by the render target. Bilinear
    /// filtering turns the fractional offset into real subpixel motion, which is what
    /// makes a very slow pan look continuous instead of stepped.
    TexF {
        tile: TileId,
        dst: RectF,
        alpha: u8,
    },
    Fill {
        rect: Rect,
        color: Color,
    },
    /// Frosted glass — see [`FrostPane`].
    /// Boxed: a pane is far larger than any other variant's payload, and `DrawCmd` is stored
    /// by value in a per-frame `Vec` that the stream path fills without ever frosting.
    Frost(Box<FrostPane>),
}

/// Blur spread, in screen px, behind every piece of glass in the app — a modal card, a grid
/// card's title strip, the submenu grown out of it, the quit dialog. One figure, so every
/// frosted surface reads as the same material. Rounded to what the compositor's chain can give.
pub const FROST_BLUR: u32 = 64;

/// Which corners of a [`FrostPane`] are rounded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corners {
    All,
    /// Bottom two only — a panel whose top edge is a straight cut across whatever it sits on
    /// (a card's title strip, the submenu grown out of it).
    Bottom,
}

/// The shape a [`FrostPane`]'s blur is cut to: a rounded rect at `radius`, on `corners`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrostMask {
    pub radius: i32,
    pub corners: Corners,
}

/// One frosted-glass pane: blur whatever this frame has already composed under it, cut the
/// blur to a rounded shape, and draw it. What a translucent surface is drawn *over* — the
/// tile that follows supplies the tint, the border and the text.
///
/// The blur source is everything earlier in the draw list, so a pane only ever sees the
/// layers beneath it.
///
/// One depth per frame: the compositor captures the frame once, at the *first* pane, and every
/// pane in that frame samples that capture. Two panes at the same depth are therefore fine (the
/// two cards of a modal cross-fade), but a pane stacked on top of another one's surface — a
/// dropdown popup over a modal card — would blur what is under the *card*, not the card. Those
/// surfaces take the glass fill without a frost; giving them one means capturing per pane.
#[derive(Clone, Copy, Debug)]
pub struct FrostPane {
    /// The shape's *unscaled* size, and the resolution its mask and blur scratch are built
    /// at. Separate from the on-screen rects so a card's focus zoom — which changes `at`
    /// every frame — rebuilds neither.
    pub shape: Size,
    /// Where the whole of `shape` lands on screen this frame, zoom included.
    pub at: Rect,
    /// The part of `at` actually drawn: a wipe's revealed window, or all of `at`.
    pub dst: Rect,
    /// The shape the blur is cut to, in `shape`'s units.
    pub mask: FrostMask,
    /// How wide the blur should be, in screen pixels of spread. The compositor picks the
    /// nearest thing its chain can give without the pane collapsing to a flat wash, so two
    /// panes that name the same figure blur alike however differently they are sized — which
    /// is what keeps a card's title strip and the submenu grown out of it looking like one
    /// surface.
    pub blur: u32,
    pub alpha: u8,
    /// The flat colour to fill the same shape with on a renderer that cannot blur (no render
    /// targets, or no composed blend mode). `None` draws nothing there — for a pane that only
    /// makes sense as a blur, and whose absence is already handled by whatever is under it.
    pub fallback: Option<Color>,
}

impl FrostPane {
    /// A pane drawn whole, unscaled and unclipped, at `mask` — every case but the card
    /// strip's wipe.
    pub fn whole(dst: Rect, mask: FrostMask, blur: u32, alpha: u8, fallback: Option<Color>) -> Self {
        Self {
            shape: Size::new(dst.width(), dst.height()),
            at: dst,
            dst,
            mask,
            blur,
            alpha,
            fallback,
        }
    }
}

pub type DrawList = Vec<DrawCmd>;

/// A width/height pair, no position — the screen's, for layout code that only needs the
/// extent it is laying out into.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}
