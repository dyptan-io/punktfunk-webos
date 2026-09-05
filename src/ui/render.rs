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

    /// `self` moved `t/255` of the way toward `other`, keeping `self`'s alpha — a shade
    /// derived from two palette entries rather than a third hex typed beside them.
    #[must_use]
    pub const fn mix(self, other: Self, t: u8) -> Self {
        const fn lerp(a: u8, b: u8, t: u8) -> u8 {
            ((a as u16 * (255 - t as u16) + b as u16 * t as u16) / 255) as u8
        }
        Self {
            r: lerp(self.r, other.r, t),
            g: lerp(self.g, other.g, t),
            b: lerp(self.b, other.b, t),
            a: self.a,
        }
    }

    /// `self` with its alpha scaled by `f` — a fill riding a fade, without the caller
    /// unpacking the colour to reach one channel.
    #[must_use]
    pub fn with_alpha_scaled(self, f: f32) -> Self {
        Self {
            a: (f32::from(self.a) * f.clamp(0.0, 1.0)) as u8,
            ..self
        }
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
    /// Subtracts `tile`'s alpha from what is already drawn, per pixel: `dst *= 1 - srcA` for
    /// colour and alpha alike, so a fully opaque mask pixel leaves the target transparent.
    ///
    /// The one command that removes rather than adds. It is how a still dissolves into the
    /// video plane underneath the graphics plane: alpha-mod is per *blit*, so shaping a fade
    /// with it means splitting the image into pieces and showing their edges, where a mask
    /// stretched over the whole image is a continuous gradient.
    #[allow(dead_code)] // The SDL compositor's arm, until the stream overlays move (WP6).
    Erase {
        tile: TileId,
        dst: Rect,
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

/// Which corners of a [`FrostPane`] are rounded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corners {
    All,
    /// Bottom two only — a panel whose top edge is a straight cut across whatever it sits on
    /// (a card's title strip, the submenu grown out of it).
    #[allow(dead_code)] // As `DrawCmd::Erase`.
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
///
/// The corollary the callers have to keep: **nothing that tints the whole screen may be pushed
/// ahead of a pane.** A modal's scrim emitted before its own pane lands in the capture, so the
/// same modal blurs a dimmed screen in the frames where its pane is the first one and an
/// undimmed screen where some earlier pane already fixed the capture — visibly two different
/// materials. Push the pane, then the scrim, then the tile: a uniform composite commutes with
/// a blur, so the scrim on its way past dims every pane identically.
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

/// Where a rasterized tile goes to be drawn. The two loops draw the same overlays (the toast,
/// the confirm dialog, the log tail) onto different backends — the menu's Skia images and the
/// stream's SDL textures — and this is the one call those overlays make.
///
/// `opaque` says the tile covers every pixel it occupies, which lets a backend skip blending.
pub trait TileSink {
    fn upload(&mut self, tile: TileId, pm: &crate::ui::painter::Painter, opaque: bool) -> anyhow::Result<()>;
}

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

/// Stretches a square nine-sliceable texture over `dst`: the four corners are drawn at
/// their own size, the four edges stretched along one axis and the middle across both.
///
/// `slice` is the side of the corner slices; whatever lies between them in the atlas (the
/// flat middle, however few pixels it is) supplies both the edges and the centre. The atlas
/// is assumed to be `2 * slice + centre` on both axes, `centre` being the leftover.
///
/// A `dst` too small to hold both corner slices on an axis has no middle to stretch, so it
/// gets one draw of the whole atlas — the honest degenerate case.
pub fn push_nine_slice(cmds: &mut DrawList, tile: TileId, atlas: u32, slice: u32, dst: Rect, alpha: u8) {
    if alpha == 0 || dst.width() == 0 || dst.height() == 0 {
        return;
    }
    let centre = atlas.saturating_sub(2 * slice);
    if centre == 0 || dst.width() < 2 * slice || dst.height() < 2 * slice {
        cmds.push(DrawCmd::TexCropped {
            tile,
            src: Rect::new(0, 0, atlas, atlas),
            dst,
            alpha,
        });
        return;
    }
    let edge = slice as i32;
    // Same three spans on both axes — the atlas is square and `dst` is sliced the same way.
    let src = [(0, slice), (edge, centre), (edge + centre as i32, slice)];
    let spans = |start: i32, len: u32| {
        [
            (start, slice),
            (start + edge, len - 2 * slice),
            (start + len as i32 - edge, slice),
        ]
    };
    let (cols, rows) = (spans(dst.x(), dst.width()), spans(dst.y(), dst.height()));
    for (&(sy, sh), &(dy, dh)) in src.iter().zip(&rows) {
        for (&(sx, sw), &(dx, dw)) in src.iter().zip(&cols) {
            cmds.push(DrawCmd::TexCropped {
                tile,
                src: Rect::new(sx, sy, sw, sh),
                dst: Rect::new(dx, dy, dw, dh),
                alpha,
            });
        }
    }
}
