//! Background frame rendering and the RAM preview cache.
//!
//! Frames render on a dedicated thread pool from immutable `Arc<Project>` snapshots, so the UI
//! never waits for the compositor. Results land in a memory-budgeted cache keyed by the comp's
//! *content* ([`comp_content`]: the comp and everything it uses), frame, scale, view and render
//! options; the timeline draws the cached range as the green cache bar. An edit keeps the frames
//! of every comp it doesn't touch, and undo finds the frames of the state it returns to.
//! Switches that don't change pixels (Audio, Lock, Shy, Hide Shy Layers) keep the frames too.
//! While playing, both memory budgets spare the next frames; while paused they use LRU.
//!
//! wasm32 has no threads: jobs wait in the same priority queue and [`Frames::pump`] hands them
//! to a [`RemoteFrames`] (the browser's frame worker, a second engine instance fed with project
//! diffs, `effectcraft_engine::remote`) or, without one, renders them on the UI thread between
//! egui frames, within a time budget.
//!
//! With the disk cache on (Settings ▸ Media & Disk Cache), every CPU-rendered frame is also
//! written to disk under a content key (the comp's content hash, frame, scale, view and render
//! options), and a job first looks there: frames survive restarts and project reopenings. The
//! timeline shows disk-only frames as a blue cache bar.
//!
//! With Mercury GPU Acceleration (a [`Gpu`] on egui-wgpu's device and the project's renderer set
//! to the GPU) frames stay on the GPU: the compositor writes an RGBA8 texture that the viewer
//! registers with egui-wgpu and draws directly; pixels are read back only when something needs
//! them (Info panel, eyedroppers, histograms).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use effectcraft_engine::project::{Comp, ItemId, ItemKind, LayerSource, Node, Project, PropGroup};
use effectcraft_engine::render::disk_cache::{self, DiskCache};
use effectcraft_engine::render::{CancellableFootage, ExprHost, FootageSource, FrameDiagnostics, LayerCache, RenderOpts, Renderer};
use effectcraft_engine::time::Tick;
use effectcraft_gpu::{DisplayFrame, Gpu};
use rayon::prelude::*;

/// Comp identities (and the project snapshots behind them) the RAM preview keeps frames of.
const MAX_KEEPERS: usize = 64;

/// GPU frames kept for RAM preview (bytes of video memory) at first. wgpu can't tell how much
/// video memory there is: each time the device runs out, the budget halves (to
/// [`MIN_GPU_BUDGET`]) and the frame renders on the CPU (#106).
const GPU_BUDGET: usize = 1 << 30;
const MIN_GPU_BUDGET: usize = 64 << 20;

/// A rendered viewer frame: CPU pixels or a GPU texture.
#[derive(Clone)]
pub enum FrameImage {
    Cpu(Arc<egui::ColorImage>),
    Gpu(Arc<DisplayFrame>),
}

impl FrameImage {
    pub fn size(&self) -> [usize; 2] {
        match self {
            FrameImage::Cpu(c) => c.size,
            FrameImage::Gpu(f) => [f.width as usize, f.height as usize],
        }
    }
    fn bytes(&self) -> usize {
        match self {
            FrameImage::Cpu(c) => c.pixels.len().saturating_mul(std::mem::size_of::<egui::Color32>()),
            FrameImage::Gpu(f) => rgba8_texture_bytes(f.texture.width(), f.texture.height(), f.texture.mip_level_count()),
        }
    }
    fn is_gpu(&self) -> bool {
        matches!(self, FrameImage::Gpu(_))
    }
}

/// Storage of RGBA8 texture levels, including narrow/non-power-of-two mip chains. Saturation
/// keeps malformed dimensions safe on both native and wasm32; no allocation is performed here.
fn rgba8_texture_bytes(mut width: u32, mut height: u32, levels: u32) -> usize {
    if width == 0 || height == 0 {
        return 0;
    }
    let mut bytes = 0usize;
    for _ in 0..levels.min(u32::BITS) {
        bytes = bytes.saturating_add((width as usize).saturating_mul(height as usize).saturating_mul(4));
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }
    bytes
}

/// A viewer frame: its comp's content, the frame, scale, view and render options. `revision` is
/// the project revision it was asked for at (job bookkeeping); it is not part of the identity, so
/// a later revision with the same content finds the frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameKey {
    pub revision: u64,
    /// The comp's content key ([`Frames::content_of`]).
    pub content: u64,
    pub comp: u64,
    pub frame: i64,
    /// Render scale × 1000.
    pub scale: u32,
    /// Hash of the 3D view camera (0 = the comp's active camera).
    pub view: u64,
    /// Hash of the other render options (Draft / Fast Previews, shadows, nested switches…).
    pub opts: u64,
}

impl FrameKey {
    /// Everything but the revision.
    fn id(&self) -> (u64, u64, i64, u32, u64, u64) {
        (self.content, self.comp, self.frame, self.scale, self.view, self.opts)
    }

    /// Whether `self` is a frame of the same comp content, scale, view and options as `base`.
    pub fn same_series(&self, base: &FrameKey) -> bool {
        FrameKey { frame: base.frame, ..*self } == *base
    }
}

impl PartialEq for FrameKey {
    fn eq(&self, o: &FrameKey) -> bool {
        self.id() == o.id()
    }
}

impl Eq for FrameKey {}

impl std::hash::Hash for FrameKey {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.id().hash(h);
    }
}

/// Where a playing preview is and what it plays next (see [`Frames::set_playhead`]): the RAM
/// preview renders the frames it reaches first before later ones, and keeps them over frames it
/// reaches later or not at all (#595).
#[derive(Clone, Copy, Debug)]
pub struct Playhead {
    /// The frame series that plays ([`FrameKey::same_series`]; its `frame` is ignored).
    pub series: FrameKey,
    /// The frame on screen.
    pub cur: i64,
    /// The preview's range, skip step and looping (as in its `PreviewPlan`).
    pub start: i64,
    pub end: i64,
    pub step: i64,
    pub looping: bool,
}

impl Playhead {
    /// How many frames the preview shows before it reaches `k` (0: on screen); `None` for
    /// frames it doesn't reach (other series, outside the range, behind it without looping).
    pub fn ahead(&self, k: &FrameKey) -> Option<i64> {
        if !k.same_series(&self.series) || k.frame < self.start || k.frame > self.end {
            return None;
        }
        let step = self.step.max(1);
        if k.frame >= self.cur {
            return Some(k.frame.saturating_sub(self.cur) / step);
        }
        // Looping: through the end of the range and around from its start.
        self.looping.then(|| (self.end.saturating_sub(self.cur) / step).saturating_add(1).saturating_add(k.frame.saturating_sub(self.start) / step))
    }
}

struct Cache {
    map: HashMap<FrameKey, FrameImage>,
    order: VecDeque<FrameKey>,
    bytes: usize,
    /// Bytes of GPU frames (also counted in `bytes`).
    gpu_bytes: usize,
    budget: usize,
    /// Bytes of GPU frames to keep at most ([`GPU_BUDGET`]).
    gpu_budget: usize,
    /// The playing preview, if any: evictions spare the frames it shows next.
    playhead: Option<Playhead>,
}

impl Cache {
    fn insert(&mut self, k: FrameKey, img: FrameImage) {
        let (sz, gpu) = (img.bytes(), img.is_gpu());
        if let Some(old) = self.map.insert(k, img) {
            self.forget(&old);
        } else {
            self.order.push_back(k);
        }
        self.bytes = self.bytes.saturating_add(sz);
        if gpu {
            self.gpu_bytes = self.gpu_bytes.saturating_add(sz);
        }
        self.evict_to_budget();
        self.evict_gpu();
    }
    /// The frame to evict next: the least recently shown, but while a preview plays, the least
    /// recently shown of the frames it doesn't reach, then the one it reaches last. Plain LRU
    /// evicted the frames just ahead of the playhead first (rendered a while ago, not shown yet)
    /// to keep the ones just shown, so a preview longer than the cache played into a gap that
    /// prefetch kept refilling while it evicted the next frames (#595).
    fn victim(&self, gpu_only: bool) -> Option<usize> {
        let mut farthest: Option<(i64, usize)> = None;
        for (i, k) in self.order.iter().enumerate() {
            if gpu_only && !self.map.get(k).is_some_and(FrameImage::is_gpu) {
                continue;
            }
            let Some(p) = &self.playhead else { return Some(i) };
            match p.ahead(k) {
                None => return Some(i),
                Some(d) if farthest.is_none_or(|(f, _)| d > f) => farthest = Some((d, i)),
                Some(_) => {}
            }
        }
        farthest.map(|(_, i)| i)
    }
    /// Video memory uses the same playhead protection, considering only GPU frames.
    fn evict_gpu(&mut self) {
        while self.gpu_bytes > self.gpu_budget {
            let Some(i) = self.victim(true) else { break };
            if let Some(old) = self.order.remove(i) {
                self.remove(&old);
            }
        }
    }
    /// The GPU ran out of memory: keep half as many GPU frames from now on.
    fn gpu_out_of_memory(&mut self) {
        self.gpu_budget = (self.gpu_budget / 2).max(MIN_GPU_BUDGET);
        self.evict_gpu();
    }
    fn forget(&mut self, img: &FrameImage) {
        self.bytes = self.bytes.saturating_sub(img.bytes());
        if img.is_gpu() {
            self.gpu_bytes = self.gpu_bytes.saturating_sub(img.bytes());
        }
    }
    fn remove(&mut self, k: &FrameKey) {
        if let Some(i) = self.map.remove(k) {
            self.forget(&i);
        }
    }
    /// Shown again: last in line for eviction.
    fn touch(&mut self, k: &FrameKey) {
        if let Some(i) = self.order.iter().rposition(|o| o == k)
            && let Some(k) = self.order.remove(i)
        {
            self.order.push_back(k);
        }
    }
    /// Drop every frame of the identities in `gone`.
    fn drop_contents(&mut self, gone: &HashSet<u64>) {
        let keys: Vec<FrameKey> = self.map.keys().filter(|k| gone.contains(&k.content)).copied().collect();
        for k in &keys {
            self.remove(k);
        }
        self.order.retain(|k| !gone.contains(&k.content));
    }
    fn evict_to_budget(&mut self) {
        while self.bytes > self.budget {
            let Some(old) = self.victim(false).and_then(|i| self.order.remove(i)) else { break };
            self.remove(&old);
        }
    }
}

/// What a render job needs (all cheap clones).
#[derive(Clone)]
pub struct RenderSource {
    pub project: Arc<Project>,
    pub footage: Arc<dyn FootageSource>,
    pub expr: Option<Arc<dyn ExprHost>>,
    pub layer_cache: Arc<LayerCache>,
    /// GPU compositor on the viewer's device (Mercury GPU Acceleration), if available.
    pub gpu: Option<Gpu>,
    /// Keep eligible GPU frames on the GPU for the viewer's native egui-wgpu textures.
    /// Channel, exposure, color conversion and region-of-interest views use CPU display pixels
    /// (`EffectcraftApp::gpu_display`).
    pub gpu_display: bool,
    /// The persistent disk cache, when enabled.
    pub disk: Option<Arc<DiskCache>>,
}

/// A [`comp_content`] identity and, when it was found in another project, that project
/// ([`Frames::identity`]).
type Identity = (u64, Option<Arc<Project>>);

/// Content keys of (project revision, comp), computed once per revision.
type ContentKeys = Arc<Mutex<HashMap<(u64, u64), u128>>>;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct ContentKeyJobs {
    running: bool,
    pending: VecDeque<(Arc<Project>, u64, u64)>,
}

fn content_key(keys: &ContentKeys, project: &Project, revision: u64, comp: u64) -> u128 {
    if let Some(k) = keys.lock().ok().and_then(|m| m.get(&(revision, comp)).copied()) {
        return k;
    }
    let k = disk_cache::comp_content_key(project, ItemId(comp));
    if let Ok(mut m) = keys.lock() {
        if m.len() > 256 {
            m.clear();
        }
        m.insert((revision, comp), k);
    }
    k
}

/// The identity of a comp's pixels in `project`, cheap enough to take at every revision: the
/// addresses of the comps it draws (an edit replaces a comp's `Arc`: `Arc::make_mut` copies it
/// while another snapshot holds it, and [`Frames::content_of`] keeps one for every identity it
/// hands out), and by value the project settings and the footage, solids and proxies it uses.
/// With expressions in its comps (which can read anything) every item counts.
pub fn comp_content(project: &Project, comp: ItemId) -> u64 {
    comp_content_with(project, comp, |_, c| Arc::as_ptr(c))
}

/// Whether two versions of a comp draw the same pixels. Markers only count when an expression
/// could read them (`markers_count`).
fn same_pixels(a: &Comp, b: &Comp, markers_count: bool) -> bool {
    if markers_count { a.same_pixels(b) } else { a.same_pixels_ignoring_markers(b) }
}

/// Whether a layer anywhere in the project has an expression, which can read markers.
fn project_has_expressions(project: &Project) -> bool {
    fn has_expression(g: &PropGroup) -> bool {
        g.children.iter().any(|c| match c {
            Node::Prop(p) => p.expr.is_some(),
            Node::Group(sub) => has_expression(sub),
        })
    }
    project.items.values().any(|i| matches!(&i.kind, ItemKind::Comp(c) if c.layers.iter().any(|l| has_expression(&l.props))))
}

/// [`comp_content`] with each comp's address given by `addr`.
fn comp_content_with(project: &Project, comp: ItemId, addr: impl Fn(ItemId, &Arc<Comp>) -> *const Comp) -> u64 {
    use std::hash::{Hash, Hasher};
    fn has_expression(g: &PropGroup) -> bool {
        g.children.iter().any(|c| match c {
            Node::Prop(p) => p.expr.is_some(),
            Node::Group(sub) => has_expression(sub),
        })
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", project.settings).hash(&mut h);
    let mut seen = BTreeSet::new();
    let mut stack = vec![comp];
    let mut expressions = false;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(ItemKind::Comp(c)) = project.item(id).map(|i| &i.kind) else { continue };
        for l in &c.layers {
            if let LayerSource::Comp { item } | LayerSource::Footage { item } | LayerSource::Solid { item } = l.source {
                stack.push(item);
            }
            expressions = expressions || has_expression(&l.props);
        }
    }
    let ids: Vec<ItemId> = if expressions { project.items.keys().copied().collect() } else { seen.into_iter().collect() };
    for id in ids {
        let Some(it) = project.item(id) else { continue };
        id.0.hash(&mut h);
        if expressions {
            // (`comp("Name")`, `footage("Name")`)
            it.name.hash(&mut h);
        }
        match &it.kind {
            ItemKind::Comp(c) => (addr(id, c) as usize).hash(&mut h),
            k => format!("{k:?}").hash(&mut h),
        }
        format!("{:?}", it.proxy).hash(&mut h);
    }
    comp.0.hash(&mut h);
    h.finish()
}

/// Hash of render options (part of [`FrameKey`] and of the disk key). The scale counts as the
/// key's thousandths ([`FrameKey::scale`]): a series rebuilt from the key (the cache bars) has
/// the same options as the frames rendered at 1/3 = 0.333… (#595).
pub fn opts_hash(o: &RenderOpts) -> u64 {
    let o = RenderOpts { scale: (o.scale * 1000.0).round() / 1000.0, ..*o };
    let mut h = disk_cache::Hash128::default();
    h.write(format!("{o:?}").as_bytes());
    h.finish() as u64
}

/// Disk key of a viewer frame (render options included: region of interest, draft…).
fn frame_disk_key(keys: &ContentKeys, project: &Project, key: &FrameKey, opts_hash: u64) -> u128 {
    let content = content_key(keys, project, key.revision, key.comp);
    disk_cache::frame_key(content, key.frame, key.scale, key.view ^ opts_hash)
}

/// A frame for a [`RemoteFrames`] renderer.
pub struct RemoteJob {
    pub project: Arc<Project>,
    pub revision: u64,
    pub comp: ItemId,
    pub t: Tick,
    pub opts: RenderOpts,
    /// The frame's disk-cache key, when the renderer keeps a disk cache
    /// ([`RemoteFrames::disk`]): served from it on a hit, stored there after a render.
    pub disk_key: Option<u128>,
}

/// Premultiplied RGBA8 pixels from a [`RemoteFrames`] renderer.
pub struct RemoteFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Render time (ms).
    pub ms: f64,
}

/// Called once with a remote frame (or why there is none).
pub type RemoteDone = Box<dyn FnOnce(Result<RemoteFrame, String>) + Send>;

/// Renders viewer frames elsewhere (the browser's frame worker), so the UI thread never does.
pub trait RemoteFrames: Send + Sync {
    /// Frames it can render at the same time (0 when it is not usable: frames then render on
    /// the UI thread again).
    fn slots(&self) -> usize;
    /// Start rendering `job`; `done` runs when the frame arrives.
    fn start(&self, job: RemoteJob, done: RemoteDone);
    /// Drop cached intermediate results (Edit ▸ Purge).
    fn purge(&self) {}
    /// It renders GPU effects (its own GPU device): frames with effects go to it rather than
    /// the UI thread's GPU compositor, which would run their effects on the CPU.
    fn gpu(&self) -> bool {
        false
    }
    /// It keeps a disk cache of frames (the browser's Origin Private File System).
    fn disk(&self) -> bool {
        false
    }
    /// The disk cache holds the frame of this key (the blue cache bar).
    fn disk_contains(&self, _key: u128) -> bool {
        false
    }
}

/// Whether a comp (or a comp it nests) has a layer with enabled effects.
pub fn comp_has_effects(project: &Project, comp: ItemId) -> bool {
    fn walk(p: &Project, comp: ItemId, depth: usize) -> bool {
        let Some(c) = p.comp(comp) else { return false };
        c.layers.iter().any(|l| {
            l.switches.effects && l.effects().is_some_and(|fx| fx.groups().any(|g| g.enabled))
                || depth < 16 && matches!(l.source, effectcraft_engine::project::LayerSource::Comp { item } if walk(p, item, depth + 1))
        })
    }
    walk(project, comp, 0)
}

pub struct Frames {
    cache: Arc<Mutex<Cache>>,
    /// Keys queued or rendering.
    inflight: Arc<Mutex<HashSet<FrameKey>>>,
    /// Pending jobs, picked by priority when a pool thread frees up (the viewer's frame first).
    queue: Arc<Mutex<Queue>>,
    /// One local whole-frame render; its decode/conversion/effects still use every pool worker.
    active_frame: Arc<AtomicBool>,
    #[cfg(not(target_arch = "wasm32"))]
    /// None if the OS refused the threads: jobs then go to rayon's global pool.
    pool: Option<rayon::ThreadPool>,
    ctx: Option<egui::Context>,
    /// Render time of the last viewer (urgent) frame in ms, for the Info panel / perf readout.
    pub last_ms: Arc<Mutex<f64>>,
    content_keys: ContentKeys,
    #[cfg(not(target_arch = "wasm32"))]
    content_key_jobs: Arc<Mutex<ContentKeyJobs>>,
    gpu_retired: Arc<AtomicBool>,
    failures: Arc<Mutex<VecDeque<(FrameKey, String)>>>,
    /// [`comp_content`] identities by (revision, comp), with the project that keeps an identity
    /// found by [`Frames::identity`] in another project.
    identities: Mutex<HashMap<(u64, u64), Identity>>,
    /// The last new identity of each comp and its project (see [`Frames::identity`]).
    bases: Mutex<HashMap<u64, (u64, Arc<Project>)>>,
    /// A project snapshot for each identity handed out, with when it was last asked for (see
    /// [`Frames::content_of`]); the counter.
    keepers: Mutex<(u64, HashMap<u64, (u64, Arc<Project>)>)>,
    /// Where frames render instead of the UI thread (wasm32), and how many it is rendering.
    remote: Option<Arc<dyn RemoteFrames>>,
    remote_busy: Arc<Mutex<usize>>,
}

impl Default for Frames {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16);
        Frames {
            cache: Arc::new(Mutex::new(Cache { map: HashMap::new(), order: VecDeque::new(), bytes: 0, gpu_bytes: 0, budget: 3 << 30, gpu_budget: GPU_BUDGET, playhead: None })),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            queue: Arc::new(Mutex::new(Queue::default())),
            active_frame: Arc::default(),
            #[cfg(not(target_arch = "wasm32"))]
            // A panicking job would abort the whole process (rayon's default for `spawn`): log it
            // and lose only that frame (the panic hook has logged the message).
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("ec-frame-{i}"))
                .panic_handler(|_| log::error!("a frame render panicked; that frame was skipped"))
                .build()
                .ok(),
            ctx: None,
            last_ms: Arc::new(Mutex::new(0.0)),
            content_keys: Arc::default(),
            #[cfg(not(target_arch = "wasm32"))]
            content_key_jobs: Arc::default(),
            gpu_retired: Arc::default(),
            failures: Arc::default(),
            identities: Mutex::default(),
            bases: Mutex::default(),
            keepers: Mutex::default(),
            remote: None,
            remote_busy: Arc::default(),
        }
    }
}

/// Premultiplied f32 → egui premultiplied Color32 (alpha kept, so the viewer can show the
/// transparency grid or the comp background colour underneath).
pub fn to_color_image(img: &effectcraft_engine::render::Image) -> egui::ColorImage {
    let px: Vec<egui::Color32> = img
        .data
        .par_iter()
        .map(|p| {
            let a = p[3].clamp(0.0, 1.0);
            let c = |v: f32| (v.clamp(0.0, a) * 255.0 + 0.5) as u8;
            egui::Color32::from_rgba_premultiplied(c(p[0]), c(p[1]), c(p[2]), (a * 255.0 + 0.5) as u8)
        })
        .collect();
    egui::ColorImage::new([img.width as usize, img.height as usize], px)
}

/// The whole factor an image of `size` is averaged down by for a texture: at least `min_k`, and
/// the smallest that fits its sides in `max_side` (frames of comps wider than the GPU's texture
/// limit, #201).
pub fn fit_factor(size: [usize; 2], max_side: usize, min_k: usize) -> usize {
    size[0].max(size[1]).div_ceil(max_side.max(1)).max(min_k).max(1)
}

/// The size `size` is shown at in a texture whose sides can't exceed `max_side`.
pub fn fitted_size(size: [usize; 2], max_side: usize) -> [usize; 2] {
    let k = fit_factor(size, max_side, 1);
    [size[0].div_ceil(k), size[1].div_ceil(k)]
}

/// The texture coordinates of the image within a texture averaged down by `k`: its last texels
/// cover fewer pixels than the factor, so drawn over the image's area with these the texels sit
/// exactly where their pixels are.
pub fn fitted_uv(size: [usize; 2], k: usize) -> egui::Rect {
    let k = k.max(1);
    let f = |n: usize| (n as f64 / (n.div_ceil(k).max(1) * k) as f64) as f32;
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(f(size[0]), f(size[1])))
}

/// `img` as a texture the renderer accepts: unchanged when it fits in `max_side`, else averaged
/// down by a whole factor (premultiplied pixels average correctly). The viewer draws it over the
/// same area, so only detail beyond the GPU's texture limit is lost, instead of the upload
/// failing (#201).
pub fn fit_texture(img: egui::ColorImage, max_side: usize) -> egui::ColorImage {
    fit_texture_by(img, max_side, 1)
}

/// [`fit_texture`] averaging down by at least `min_k` (see [`fit_factor`]).
pub fn fit_texture_by(img: egui::ColorImage, max_side: usize, min_k: usize) -> egui::ColorImage {
    let k = fit_factor(img.size, max_side, min_k);
    if k == 1 {
        return img;
    }
    average_down(&img, k)
}

/// A shared frame as texture data (see [`fit_texture_by`]): the frame itself when it needs no
/// averaging, without copying its pixels. The viewer puts every frame a preview shows on screen;
/// copying a 4K frame first held the UI thread for about a millisecond each time (#595).
pub fn fit_shared_texture_by(img: &Arc<egui::ColorImage>, max_side: usize, min_k: usize) -> egui::ImageData {
    let k = fit_factor(img.size, max_side, min_k);
    if k == 1 {
        return egui::ImageData::Color(img.clone());
    }
    average_down(img, k).into()
}

/// `img` averaged down by `k` (rounded up at the edges).
fn average_down(img: &egui::ColorImage, k: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let (fw, fh) = (w.div_ceil(k), h.div_ceil(k));
    let px: Vec<egui::Color32> = (0..fw * fh)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = ((i % fw) * k, (i / fw) * k);
            let (mut sum, mut n) = ([0u64; 4], 0u64);
            for y in y0..(y0 + k).min(h) {
                for c in img.pixels.get(y * w + x0..y * w + (x0 + k).min(w)).unwrap_or_default() {
                    for (s, v) in sum.iter_mut().zip(c.to_array()) {
                        *s += v as u64;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            let [r, g, b, a] = sum.map(|s| ((s + n / 2) / n) as u8);
            egui::Color32::from_rgba_premultiplied(r, g, b, a)
        })
        .collect();
    egui::ColorImage::new([fw, fh], px)
}

/// The largest texture side the renderer accepts.
pub fn max_texture_side(ctx: &egui::Context) -> usize {
    ctx.input(|i| i.max_texture_side)
}

/// `ctx.load_texture` for frame-sized images: fitted to the renderer's texture limit first.
pub fn load_fitted(ctx: &egui::Context, name: impl Into<String>, img: egui::ColorImage, opts: egui::TextureOptions) -> egui::TextureHandle {
    ctx.load_texture(name, fit_texture(img, max_texture_side(ctx)), opts)
}

impl Frames {
    /// Retire only GPU preview state; documents, CPU frames and running job leases survive.
    /// Publication checks the same latch while holding the cache lock, so a late GPU frame
    /// cannot restore an invalid texture after this removal.
    pub fn retire_gpu(&self) {
        self.gpu_retired.store(true, Ordering::Release);
        if let Ok(mut cache) = self.cache.lock() {
            let keys: Vec<_> = cache.map.iter().filter_map(|(key, image)| image.is_gpu().then_some(*key)).collect();
            for key in keys {
                cache.remove(&key);
            }
            let Cache { map, order, .. } = &mut *cache;
            order.retain(|key| map.contains_key(key));
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// Native parallel-loop workers, including the OS fallback pool. A whole frame uses
    /// these workers together; their count is not a simultaneous-frame limit.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parallelism(&self) -> usize {
        self.pool.as_ref().map_or_else(rayon::current_num_threads, rayon::ThreadPool::current_num_threads)
    }
    #[cfg(target_arch = "wasm32")]
    pub fn parallelism(&self) -> usize {
        self.remote_slots().max(1)
    }
    pub fn set_context(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
    }

    /// The egui context (to request repaints from callbacks).
    pub fn context(&self) -> Option<egui::Context> {
        self.ctx.clone()
    }

    /// The cached frame, which becomes the most recently used.
    pub fn get(&self, k: &FrameKey) -> Option<FrameImage> {
        let mut c = self.cache.lock().ok()?;
        let img = c.map.get(k).cloned()?;
        c.touch(k);
        Some(img)
    }

    /// Prefer the exact requested frame. While an edit renders, advance to the newest completed
    /// revision of the same comp, time, scale, view and render options instead of freezing on the
    /// image from before the drag. Input may arrive before the viewer draws on every UI frame,
    /// so even a fast worker cannot always finish the *latest* revision in time.
    ///
    /// Never move backwards when workers finish out of order, and never substitute another time
    /// or view while scrubbing or switching comps. The exact content lookup above also means an
    /// undo can immediately reuse an old cache entry, regardless of its original revision.
    pub fn get_for_viewer(&self, wanted: &FrameKey, shown: Option<&FrameKey>) -> Option<(FrameKey, FrameImage)> {
        if let Some(img) = self.get(wanted) {
            return Some((*wanted, img));
        }
        let shown = shown?;
        if shown.comp != wanted.comp || shown.frame != wanted.frame || shown.view != wanted.view || shown.revision >= wanted.revision {
            return None;
        }
        let mut c = self.cache.lock().ok()?;
        let newest = c
            .map
            .keys()
            .filter(|k| {
                k.revision > shown.revision
                    && k.revision < wanted.revision
                    && k.comp == wanted.comp
                    && k.frame == wanted.frame
                    && k.scale == wanted.scale
                    && k.view == wanted.view
                    && k.opts == wanted.opts
            })
            .max_by_key(|k| k.revision)
            .copied()?;
        let img = c.map.get(&newest).cloned()?;
        c.touch(&newest);
        Some((newest, img))
    }

    /// The [`comp_content`] identity of `comp` in `project` at `revision` (taken once per
    /// revision). The project is kept while frames of that identity are cached or rendering, so
    /// the comp addresses it hashes can't be reused by other comps meanwhile.
    pub fn content_of(&self, project: &Arc<Project>, revision: u64, comp: ItemId) -> u64 {
        let known = self.identities.lock().ok().and_then(|m| m.get(&(revision, comp.0)).cloned());
        let (id, base) = known.unwrap_or_else(|| {
            let found = self.identity(project, comp);
            if let Ok(mut m) = self.identities.lock() {
                if m.len() > 256 {
                    m.clear();
                }
                m.insert((revision, comp.0), found.clone());
            }
            found
        });
        if let Ok(mut guard) = self.keepers.lock() {
            let (clock, k) = &mut *guard;
            *clock += 1;
            let now = *clock;
            k.entry(id).or_insert_with(|| (now, base.unwrap_or_else(|| project.clone()))).0 = now;
            if k.len() > MAX_KEEPERS {
                let inflight: HashSet<u64> = self.inflight.lock().map(|s| s.iter().map(|f| f.content).collect()).unwrap_or_default();
                if let Ok(mut c) = self.cache.lock() {
                    // Snapshots of identities without frames go; past the limit the least
                    // recently asked for go too, with their frames (a long drag leaves many).
                    let cached: HashSet<u64> = c.map.keys().map(|f| f.content).collect();
                    k.retain(|i, _| *i == id || cached.contains(i) || inflight.contains(i));
                    if k.len() > MAX_KEEPERS {
                        let mut ages: Vec<(u64, u64)> = k.iter().filter(|(i, _)| **i != id && !inflight.contains(i)).map(|(i, (t, _))| (*t, *i)).collect();
                        ages.sort_unstable();
                        let gone: HashSet<u64> = ages.into_iter().take(k.len().saturating_sub(MAX_KEEPERS / 2)).map(|(_, i)| i).collect();
                        k.retain(|i, _| !gone.contains(i));
                        c.drop_contents(&gone);
                    }
                }
            }
        }
        id
    }

    /// The [`comp_content`] identity of `comp` in `project`, or the comp's last new identity when
    /// every comp it draws is the same or draws the same pixels there ([`Comp::same_pixels`]:
    /// only Audio, Lock or Shy switches changed), so toggling those keeps the cached frames
    /// (#103). That identity hashes the addresses of the comps in its project, which is
    /// returned to keep them.
    fn identity(&self, project: &Arc<Project>, comp: ItemId) -> Identity {
        let plain = comp_content(project, comp);
        let Ok(mut bases) = self.bases.lock() else { return (plain, None) };
        // Markers can only change pixels through expressions, which read them.
        let markers_count = project_has_expressions(project);
        if let Some((base_id, base)) = bases.get(&comp.0)
            && *base_id != plain
            && comp_content_with(project, comp, |id, c| match base.item(id).map(|i| &i.kind) {
                Some(ItemKind::Comp(b)) if Arc::ptr_eq(b, c) || same_pixels(b, c, markers_count) => Arc::as_ptr(b),
                _ => Arc::as_ptr(c),
            }) == *base_id
        {
            return (*base_id, Some(base.clone()));
        }
        if bases.len() > MAX_KEEPERS {
            bases.clear();
        }
        bases.insert(comp.0, (plain, project.clone()));
        (plain, None)
    }

    pub fn is_cached(&self, k: &FrameKey) -> bool {
        self.cache.lock().map(|c| c.map.contains_key(k)).unwrap_or(false)
    }

    pub fn inflight(&self) -> usize {
        self.inflight.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Cached frame numbers of the series of `base` (its comp content, scale, view and render
    /// options) — for the cache bar.
    pub fn cached_frames(&self, base: &FrameKey) -> Vec<i64> {
        let Ok(c) = self.cache.lock() else { return vec![] };
        let mut v: Vec<i64> = c.map.keys().filter(|k| k.same_series(base)).map(|k| k.frame).collect();
        v.sort_unstable();
        v
    }

    /// Frames of the series of `base` in the disk cache but not in RAM — the blue cache bar.
    /// `opts` are the viewer's render options (part of the disk key). Native queries return
    /// no blue frames until the content key is ready; hashing never blocks timeline painting.
    pub fn disk_frames(&self, src: &RenderSource, base: &FrameKey, frames: i64, opts: &RenderOpts) -> Vec<i64> {
        let remote = self.remote.as_ref().filter(|r| r.disk());
        let contains = |k: u128| match (&src.disk, remote) {
            (Some(dc), _) => dc.contains(disk_cache::Kind::Frame, k),
            (None, Some(r)) => r.disk_contains(k),
            (None, None) => false,
        };
        if src.disk.is_none() && remote.is_none() {
            return vec![];
        }
        #[cfg(not(target_arch = "wasm32"))]
        let content = {
            let known = self.content_keys.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(base.revision, base.comp)).copied();
            let Some(content) = known else {
                self.request_content_key(src.project.clone(), base.revision, base.comp);
                return vec![];
            };
            content
        };
        #[cfg(target_arch = "wasm32")]
        let content = content_key(&self.content_keys, &src.project, base.revision, base.comp);
        let ram: HashSet<i64> = self.cached_frames(base).into_iter().collect();
        let oh = opts_hash(opts);
        (0..frames.clamp(0, 100_000))
            .filter(|f| !ram.contains(f))
            .filter(|f| contains(disk_cache::frame_key(content, *f, base.scale, base.view ^ oh)))
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn request_content_key(&self, project: Arc<Project>, revision: u64, comp: u64) {
        let mut jobs = self.content_key_jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Coalesce rapid edits. One worker hashes at most one revision at a time; retained
        // snapshots are bounded even with multiple timelines open.
        jobs.pending.retain(|(_, _, c)| *c != comp);
        if jobs.pending.len() >= 8 {
            jobs.pending.pop_front();
        }
        jobs.pending.push_back((project, revision, comp));
        if jobs.running {
            return;
        }
        jobs.running = true;
        drop(jobs);
        let (jobs, keys, ctx) = (self.content_key_jobs.clone(), self.content_keys.clone(), self.ctx.clone());
        let work = move || loop {
            let next = {
                let mut jobs = jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let next = jobs.pending.pop_back();
                if next.is_none() {
                    jobs.running = false;
                }
                next
            };
            let Some((project, revision, comp)) = next else { return };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| content_key(&keys, &project, revision, comp))).is_err() {
                log::error!("could not compute a preview disk-cache key; the cache indicator will retry");
            }
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        };
        match &self.pool {
            Some(pool) => pool.spawn(work),
            None => rayon::spawn(work),
        }
    }

    /// Change the RAM preview cache budget (Settings ▸ Memory & CPU), evicting the oldest
    /// frames when it shrinks.
    pub fn set_budget(&self, bytes: usize) {
        if let Ok(mut c) = self.cache.lock() {
            c.budget = bytes;
            c.evict_to_budget();
        }
    }

    pub fn budget(&self) -> usize {
        self.cache.lock().map(|c| c.budget).unwrap_or(0)
    }

    /// Frames that fit in the active display cache, with 10% headroom. A GPU preview is bounded
    /// by both budgets and includes every display mip; otherwise cache-before-playback could
    /// wait forever for a range larger than the cache can retain.
    pub(crate) fn preview_capacity(&self, width: u32, height: u32, gpu: bool) -> i64 {
        let levels = if gpu { u32::BITS - width.max(height).max(1).leading_zeros() } else { 1 };
        let bytes = rgba8_texture_bytes(width, height, levels).max(1);
        let c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let budget = if gpu { c.budget.min(c.gpu_budget) } else { c.budget };
        i64::try_from(budget.saturating_sub(budget / 10) / bytes).unwrap_or(i64::MAX).max(1)
    }

    /// A terminal render failure for this exact content/time/options. Edits naturally select
    /// another key; until then automatic repaint/prefetch must not retry the rejected job.
    pub fn failure(&self, key: &FrameKey) -> Option<String> {
        self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|(k, _)| k == key).map(|(_, message)| message.clone())
    }

    /// Register a synchronous auxiliary-view failure using the same bounded UI state.
    pub fn record_failure(&self, key: FrameKey, error: String) {
        self.worker().fail(key, false, error);
    }

    pub fn retry(&self, key: &FrameKey) {
        self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(k, _)| k != key);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    pub fn clear(&self) {
        self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        if let Some(r) = &self.remote {
            r.purge();
        }
        if let Ok(mut c) = self.cache.lock() {
            c.map.clear();
            c.order.clear();
            c.bytes = 0;
            c.gpu_bytes = 0;
        }
        // Disk keys hash the footage files' size and time, but are remembered per revision; a
        // file reloaded from disk without a new revision (a re-rendered image sequence) must be
        // keyed again, or the disk cache serves the frames from before.
        if let Ok(mut k) = self.content_keys.lock() {
            k.clear();
        }
    }

    /// The playing preview's position (`None` when no preview plays): prefetch renders the
    /// frames it reaches first before the others, evictions spare the frames it shows next, and
    /// queued prefetch frames of its series it no longer reaches are dropped (#595).
    pub fn set_playhead(&self, playhead: Option<Playhead>) {
        if let Ok(mut c) = self.cache.lock() {
            c.playhead = playhead;
        }
        let Ok(mut q) = self.queue.lock() else { return };
        q.playhead = playhead;
        let Some(p) = playhead else { return };
        let Ok(mut inf) = self.inflight.lock() else { return };
        q.jobs.retain(|j| {
            let gone = !j.urgent && j.key.same_series(&p.series) && p.ahead(&j.key).is_none();
            if gone {
                inf.remove(&j.key);
            }
            !gone
        });
    }

    /// Queue a prefetch frame (no-op if cached or already queued/rendering).
    pub fn request(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts) {
        self.request_with(src, key, comp, t, opts, false);
    }

    /// Queue the frame the viewer is showing: it jumps ahead of every prefetch job.
    pub fn request_urgent(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts) {
        self.request_with(src, key, comp, t, opts, true);
    }

    /// An urgent (viewer) frame is queued or rendering.
    pub fn urgent_pending(&self) -> bool {
        self.queue.lock().map(|q| q.urgent_running > 0 || q.jobs.iter().any(|j| j.urgent)).unwrap_or(false)
    }

    fn request_with(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts, urgent: bool) {
        if self.is_cached(&key) || self.failure(&key).is_some() {
            return;
        }
        let Ok(mut q) = self.queue.lock() else { return };
        let Ok(mut inf) = self.inflight.lock() else { return };
        // Jobs for an older project revision can never be shown: drop them.
        let before = q.jobs.len();
        q.jobs.retain(|j| j.key.revision >= key.revision);
        if q.jobs.len() < before {
            inf.retain(|k| k.revision >= key.revision || q.running.contains(k));
        }
        if urgent {
            // Only the newest viewer frame is urgent. The comp's earlier viewer frames that
            // haven't started are dropped (scrubbing: rendering every frame the pointer passed
            // would hold the workers the frame under it needs); frames already rendering finish
            // into the cache. During edits the viewer advances through compatible completed
            // revisions; scrubbing holds its last frame until the requested time is in (#284).
            // Other comps' viewer frames become prefetch.
            q.jobs.retain(|j| {
                let superseded = j.urgent && j.key != key && j.key.comp == key.comp;
                if superseded {
                    inf.remove(&j.key);
                }
                !superseded
            });
            for j in q.jobs.iter_mut() {
                j.urgent = j.key == key;
            }
            // The comp's viewer frames of other times that are rendering are called off too: a
            // movie seeking for each of them held the decoder the frame under the pointer
            // waited for, so it came seconds after the pointer stopped (#329). Another revision
            // of the same frame (a live edit) finishes, as the viewer shows it meanwhile.
            for (k, cancel) in &q.urgent_cancels {
                if k.comp == key.comp && k.frame != key.frame {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        if !inf.insert(key) {
            return;
        }
        q.seq += 1;
        let seq = q.seq;
        q.jobs.push(Job { key, src: src.clone(), comp, t, opts, urgent, seq, cancel: Arc::default() });
        drop(inf);
        drop(q);
        #[cfg(not(target_arch = "wasm32"))]
        if self.remote.is_none() {
            let w = self.worker();
            // One spawn per job; each spawn renders whichever queued jobs matter most right now.
            let job = move || w.drain();
            match &self.pool {
                Some(pool) => pool.spawn(job),
                None => rayon::spawn(job),
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(ctx) = &self.ctx {
            // rendered by `pump` after this UI frame
            ctx.request_repaint();
        }
    }

    fn worker(&self) -> Worker {
        Worker {
            queue: self.queue.clone(),
            active_frame: self.active_frame.clone(),
            cache: self.cache.clone(),
            inflight: self.inflight.clone(),
            ctx: self.ctx.clone(),
            last: self.last_ms.clone(),
            content_keys: self.content_keys.clone(),
            gpu_retired: self.gpu_retired.clone(),
            failures: self.failures.clone(),
        }
    }

    /// Render frames with `remote` from now on (`None`: on this thread again).
    pub fn set_remote(&mut self, remote: Option<Arc<dyn RemoteFrames>>) {
        self.remote = remote;
    }

    /// The remote renderer keeps a disk cache (the browser's).
    pub fn remote_disk(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.disk())
    }

    /// Whether frames go to a usable [`RemoteFrames`].
    pub fn remote_active(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.slots() > 0)
    }

    /// Frames the remote renderer renders at once (0 without one).
    pub fn remote_slots(&self) -> usize {
        self.remote.as_ref().map(|r| r.slots()).unwrap_or(0)
    }

    /// Frames being rendered remotely.
    pub fn remote_busy(&self) -> usize {
        self.remote_busy.lock().map(|b| *b).unwrap_or(0)
    }

    /// Hand queued frames to the remote renderer while it has free slots, most important first
    /// (the viewer's frame, then prefetch in request order; a viewer frame requested while every
    /// slot is busy goes to the first one that frees up). Frames that the GPU compositor can keep
    /// on the GPU render here instead (no readback). Returns how many were started.
    pub fn dispatch_remote(&self) -> usize {
        let Some(remote) = self.remote.clone() else { return 0 };
        let slots = remote.slots();
        let w = self.worker();
        let mut started = 0;
        loop {
            let busy = self.remote_busy();
            if busy >= slots {
                break;
            }
            let job = {
                let Ok(mut q) = self.queue.lock() else { break };
                let Some(i) = q.best() else { break };
                let job = q.jobs.swap_remove(i);
                q.running.insert(job.key);
                if job.urgent {
                    q.urgent_running += 1;
                }
                job
            };
            // The GPU compositor here keeps the frame on the GPU, but runs layer effects on the
            // CPU (this thread can't wait for readbacks): frames with effects go to workers
            // that render them on their own GPU.
            let gpu_here = job.src.gpu.is_some() && job.src.gpu_display && !(remote.gpu() && comp_has_effects(&job.src.project, job.comp));
            if gpu_here {
                let t0 = web_time::Instant::now();
                if let Some(img) = w.render_gpu(&job) {
                    w.finish(&job, img, t0);
                    continue;
                }
            }
            if let Ok(mut b) = self.remote_busy.lock() {
                *b += 1;
            }
            started += 1;
            let (key, urgent) = (job.key, job.urgent);
            let fw = self.worker();
            let busy = self.remote_busy.clone();
            let t0 = web_time::Instant::now();
            let disk_key = remote.disk().then(|| frame_disk_key(&w.content_keys, &job.src.project, &key, opts_hash(&job.opts)));
            let rj = RemoteJob { project: job.src.project.clone(), revision: key.revision, comp: job.comp, t: job.t, opts: job.opts, disk_key };
            remote.start(
                rj,
                Box::new(move |r: Result<RemoteFrame, String>| {
                    if let Ok(mut b) = busy.lock() {
                        *b = b.saturating_sub(1);
                    }
                    match r {
                        Ok(f) => {
                            let img = egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.rgba);
                            fw.finish_key(key, urgent, FrameImage::Cpu(Arc::new(img)), t0);
                        }
                        Err(e) if e.starts_with("Render failed in comp ") => fw.fail(key, urgent, e),
                        Err(e) => {
                            // Transport/stale-revision misses remain retryable. A contextual
                            // render failure is terminal for this content/time/options key.
                            log::debug!("remote frame: {e}");
                            fw.release(key, urgent);
                        }
                    }
                }),
            );
        }
        started
    }

    /// Render queued frames, most important first. With a usable [`RemoteFrames`] they are only
    /// handed over ([`Frames::dispatch_remote`]); otherwise they render on this thread until the
    /// queue is empty or `budget` has passed (at least one frame). Returns whether frames are
    /// still queued or rendering remotely. Only wasm32 needs this (native frames render on the
    /// pool); elsewhere it does nothing.
    pub fn pump(&self, budget: std::time::Duration) -> bool {
        if cfg!(not(target_arch = "wasm32")) {
            return false;
        }
        if self.remote_active() {
            self.dispatch_remote();
            return self.queue.lock().map(|q| !q.jobs.is_empty()).unwrap_or(false) || self.remote_busy() > 0;
        }
        let t0 = web_time::Instant::now();
        let w = self.worker();
        while w.run_next() {
            if t0.elapsed() >= budget {
                break;
            }
        }
        self.queue.lock().map(|q| !q.jobs.is_empty()).unwrap_or(false)
    }
}

/// Renders queued jobs (on a pool thread, or the UI thread on wasm32).
struct Worker {
    queue: Arc<Mutex<Queue>>,
    active_frame: Arc<AtomicBool>,
    cache: Arc<Mutex<Cache>>,
    inflight: Arc<Mutex<HashSet<FrameKey>>>,
    ctx: Option<egui::Context>,
    last: Arc<Mutex<f64>>,
    content_keys: ContentKeys,
    gpu_retired: Arc<AtomicBool>,
    failures: Arc<Mutex<VecDeque<(FrameKey, String)>>>,
}

/// Release the admission slot even if an unexpected panic escapes a frame's safety boundary.
struct ActiveFrame<'a>(&'a AtomicBool);

impl Drop for ActiveFrame<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// This pool thread is inside [`Worker::drain`] (rendering a frame).
    static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Clears [`DRAINING`] when the drain ends, even by a panic.
#[cfg(not(target_arch = "wasm32"))]
struct Draining;

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Draining {
    fn drop(&mut self) {
        DRAINING.set(false);
    }
}

impl Worker {
    /// Render queued jobs, most important first, until the queue is empty.
    ///
    /// A pool thread waiting inside a render (on a parallel loop's join) runs other pool work
    /// meanwhile, including the spawns of newer requests: each would start a whole frame render
    /// on top of the one waiting, with all of its buffers. Dragging a layer asks for a new frame
    /// every UI frame, so renders piled up on every thread until memory ran out (#647). A spawn
    /// that lands inside a render returns at once instead; the render below it takes the queued
    /// jobs when it is done. A shared admission slot also prevents other threads from starting
    /// whole frames while the active frame's decode/conversion/effects use those workers.
    #[cfg(not(target_arch = "wasm32"))]
    fn drain(&self) {
        if DRAINING.get() {
            return;
        }
        DRAINING.set(true);
        let _reset = Draining;
        while self.run_next() {}
    }

    /// Render the queued job that matters most right now. `false` when empty or already busy.
    fn run_next(&self) -> bool {
        let Worker { queue, content_keys, .. } = self;
        let (job, _active) = {
            let Ok(mut q) = queue.lock() else { return false };
            let Some(i) = q.best() else { return false };
            // Acquire while the queue is locked and nonempty: otherwise a request could
            // arrive after an empty check, see a busy slot, and be left without a drain.
            if self.active_frame.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
                return false;
            }
            let active = ActiveFrame(&self.active_frame);
            let job = q.jobs.swap_remove(i);
            q.running.insert(job.key);
            if job.urgent {
                q.urgent_running += 1;
                q.urgent_cancels.push((job.key, job.cancel.clone()));
            }
            (job, active)
        };
        let t0 = web_time::Instant::now();
        // Cache reads, key preparation and conversion need the same last-resort boundary as
        // rendering. A bad cache cannot strand this job or block every later viewer request.
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
            let disk = job.src.disk.as_ref().map(|dc| (dc.clone(), frame_disk_key(content_keys, &job.src.project, &job.key, opts_hash(&job.opts))));
            let from_disk = disk.as_ref().and_then(|(dc, k)| dc.get_frame(*k));
            let disk_hit = from_disk.is_some();
            let ci = match from_disk {
                Some(f) => FrameImage::Cpu(Arc::new(egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.rgba))),
                None => self.render_guarded(&job)?,
            };
            if job.cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            // Wake the viewer before optional cache conversion/compression. The admission
            // slot stays held until that work ends, bounding whole-frame live storage too.
            self.finish(&job, ci.clone(), t0);
            Ok(Some((ci, disk.filter(|_| !disk_hit))))
        }));
        let (ci, disk) = match prepared {
            Ok(Ok(Some(prepared))) => prepared,
            Ok(Ok(None)) => {
                self.release(job.key, job.urgent);
                return true;
            }
            Ok(Err(error)) => {
                self.fail(job.key, job.urgent, error);
                return true;
            }
            Err(_) => {
                self.fail(job.key, job.urgent, "Preparing the preview failed unexpectedly; purge the disk cache or retry this frame".into());
                return true;
            }
        };
        if let (Some((dc, k)), FrameImage::Cpu(img)) = (&disk, &ci) {
            let persisted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                let bytes = img.pixels.len().checked_mul(4).ok_or("disk frame byte count overflow")?;
                let mut rgba = Vec::new();
                rgba.try_reserve_exact(bytes).map_err(|e| format!("cannot allocate disk frame: {e}"))?;
                for pixel in &img.pixels {
                    rgba.extend_from_slice(&pixel.to_array());
                }
                dc.put_frame(*k, img.size[0] as u32, img.size[1] as u32, &rgba);
                Ok(())
            }));
            match persisted {
                Ok(Ok(())) => {}
                Ok(Err(e)) => log::warn!("preview frame {} remains in RAM; disk cache write skipped: {e}", job.key.frame),
                Err(_) => log::warn!("preview frame {} remains in RAM; disk cache write failed unexpectedly", job.key.frame),
            }
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
        true
    }

    fn fail(&self, key: FrameKey, urgent: bool, error: String) {
        let mut failures = self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        failures.retain(|(k, _)| *k != key);
        if failures.len() >= MAX_KEEPERS {
            failures.pop_front();
        }
        failures.push_back((key, error.chars().take(4096).collect()));
        drop(failures);
        self.release(key, urgent);
    }

    /// Store a finished frame and release its job.
    fn finish(&self, job: &Job, img: FrameImage, t0: web_time::Instant) {
        self.finish_key(job.key, job.urgent, img, t0);
    }

    fn finish_key(&self, key: FrameKey, urgent: bool, img: FrameImage, t0: web_time::Instant) {
        if urgent && let Ok(mut l) = self.last.lock() {
            *l = t0.elapsed().as_secs_f64() * 1000.0;
        }
        if let Ok(mut c) = self.cache.lock()
            && (!img.is_gpu() || !self.gpu_retired.load(Ordering::Acquire))
        {
            c.insert(key, img);
        }
        self.release(key, urgent);
    }

    /// The job of `key` is no longer queued or running (rendered or dropped).
    fn release(&self, key: FrameKey, urgent: bool) {
        if let Ok(mut inf) = self.inflight.lock() {
            inf.remove(&key);
        }
        if let Ok(mut q) = self.queue.lock() {
            let running = q.running.remove(&key);
            if running && urgent {
                q.urgent_running = q.urgent_running.saturating_sub(1);
                if let Some(i) = q.urgent_cancels.iter().position(|(k, _)| *k == key) {
                    q.urgent_cancels.swap_remove(i);
                }
            }
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// Last-resort unwind guard; checked effect errors remain visible without repeated jobs.
    fn render_guarded(&self, job: &Job) -> Result<FrameImage, String> {
        let attempt = |gpu| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.render_job(job, gpu)));
        match attempt(true) {
            Ok(result) => result,
            Err(_) => {
                log::error!("a frame render panicked (frame {})", job.key.frame);
                if job.src.gpu.is_some() {
                    attempt(false).map_err(|_| "Rendering failed unexpectedly; edit the project or retry this frame".to_string())?
                } else {
                    Err("Rendering failed unexpectedly; edit the project or retry this frame".into())
                }
            }
        }
    }

    /// A failed scoped attempt retries on CPU. Retired contexts cannot recover by reducing
    /// their budget; only a healthy device's allocation failure uses the upstream backoff.
    fn gpu_attempt_failed(&self, gpu: &Gpu, e: &str) {
        if gpu.context().check_health().is_err() {
            log::warn!("the GPU context is retired ({e}); the frame renders on the CPU");
            return;
        }
        log::warn!("the GPU ran out of memory ({e}); the frame renders on the CPU and fewer GPU frames are kept");
        if let Ok(mut c) = self.cache.lock() {
            c.gpu_out_of_memory();
        }
    }

    /// The frame on the GPU, kept there for the viewer, if the compositor can do it alone.
    fn render_gpu(&self, job: &Job) -> Option<FrameImage> {
        if self.gpu_retired.load(Ordering::Acquire) {
            return None;
        }
        let g = job.src.gpu.as_ref()?;
        let footage = job.footage();
        let diagnostics = FrameDiagnostics::default();
        let mut r = Renderer::new(&job.src.project, &footage, job.opts);
        r.diagnostics = Some(&diagnostics);
        r.expr = job.src.expr.as_deref();
        r.cache = Some(&job.src.layer_cache);
        r.accel = Some(g as &dyn effectcraft_engine::render::Accelerator);
        r.active_accel()?;
        match g.within_memory(|| g.render_display(&r, job.comp, job.t)) {
            Ok(f) => diagnostics.result(f).ok().flatten().map(|f| FrameImage::Gpu(Arc::new(f))),
            Err(e) => {
                self.gpu_attempt_failed(g, &e);
                None
            }
        }
    }

    /// Render a frame; `gpu: false` keeps it off the GPU compositor.
    fn render_job(&self, job: &Job, gpu: bool) -> Result<FrameImage, String> {
        let footage = job.footage();
        let diagnostics = FrameDiagnostics::default();
        let mut r = Renderer::new(&job.src.project, &footage, job.opts);
        r.diagnostics = Some(&diagnostics);
        r.expr = job.src.expr.as_deref();
        let gpu_allowed = gpu && !self.gpu_retired.load(Ordering::Acquire);
        // A failed GPU attempt may have populated intermediate entries before its scope
        // reported failure. The CPU retry must compute its own pixels from the snapshot.
        r.cache = gpu_allowed.then_some(job.src.layer_cache.as_ref());
        let gpu = job.src.gpu.as_ref().filter(|_| gpu_allowed);
        r.accel = gpu.map(|g| g as &dyn effectcraft_engine::render::Accelerator);
        // The GPU leaves the frame in a texture for the viewer; otherwise (Software Only, no
        // adapter, or a frame the GPU cannot finish here) the CPU renders it.
        // Auto (Mercury GPU Acceleration) shows each comp on whichever compositor measured
        // faster for it (light comps composite faster on the CPU).
        if let (Some(g), Some(a)) = (gpu, r.active_accel())
            && job.src.gpu_display
        {
            // The browser cannot wait for the GPU to time it (and its CPU path is slow): always
            // the GPU there.
            let auto = if cfg!(target_arch = "wasm32") { None } else { r.frame_auto(a, job.comp, true) };
            if auto.is_none_or(|(p, key)| p.choose(key)) {
                let t0 = web_time::Instant::now();
                match g.within_memory(|| g.render_display(&r, job.comp, job.t)) {
                    Ok(Some(f)) => {
                        if diagnostics.failed() {
                            return self.render_job(job, false);
                        }
                        if let Some((p, key)) = auto {
                            // Time the GPU's work, not just its recording.
                            g.wait();
                            if g.context().check_health().is_err() {
                                return self.render_job(job, false);
                            }
                            p.record(key, true, t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        return Ok(FrameImage::Gpu(Arc::new(f)));
                    }
                    Ok(None) => {
                        if diagnostics.failed() {
                            return self.render_job(job, false);
                        }
                        if let Some((p, key)) = auto {
                            p.declined(key);
                        }
                    }
                    Err(e) => {
                        self.gpu_attempt_failed(g, &e);
                        return self.render_job(job, false);
                    }
                }
            }
            let t0 = web_time::Instant::now();
            let image = r.comp_frame_cpu_checked(job.comp, job.t).map_err(|error| error.to_string())?;
            let img = to_color_image(&image);
            if let Some((p, key)) = auto {
                p.record(key, false, t0.elapsed().as_secs_f64() * 1000.0);
            }
            return Ok(FrameImage::Cpu(Arc::new(img)));
        }
        if let Some(g) = gpu {
            // The GPU composites and the frame is read back.
            return match g.within_memory(|| r.comp_frame_checked(job.comp, job.t)) {
                Ok(result) => result.map(|image| FrameImage::Cpu(Arc::new(to_color_image(&image)))).map_err(|error| error.to_string()),
                Err(e) => {
                    self.gpu_attempt_failed(g, &e);
                    self.render_job(job, false)
                }
            };
        }
        r.comp_frame_checked(job.comp, job.t).map(|image| FrameImage::Cpu(Arc::new(to_color_image(&image)))).map_err(|error| error.to_string())
    }
}

struct Job {
    key: FrameKey,
    src: RenderSource,
    comp: ItemId,
    t: Tick,
    opts: RenderOpts,
    urgent: bool,
    seq: u64,
    /// Set when the viewer no longer waits for this frame (see [`Frames::request_urgent`]).
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// The footage this job's render reads, called off with the job.
    fn footage(&self) -> CancellableFootage<'_> {
        CancellableFootage { inner: self.src.footage.as_ref(), cancel: self.cancel.clone() }
    }
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    running: HashSet<FrameKey>,
    urgent_running: usize,
    /// The running viewer (urgent) frames and how to call them off.
    urgent_cancels: Vec<(FrameKey, Arc<AtomicBool>)>,
    seq: u64,
    /// The playing preview, if any: its prefetch renders nearest first.
    playhead: Option<Playhead>,
}

impl Queue {
    /// The job that matters most: urgent first (newest), then, while a preview plays, the
    /// prefetch frame it reaches first, then prefetch in request order. In request order alone,
    /// frames asked for again after an eviction (the ones right ahead of the playhead) waited
    /// behind every frame further ahead (#595).
    fn best(&self) -> Option<usize> {
        let p = self.playhead.as_ref();
        self.jobs
            .iter()
            .enumerate()
            .max_by_key(|(_, j)| {
                let near = if j.urgent { None } else { p.and_then(|p| p.ahead(&j.key)) };
                (j.urgent, near.is_some(), near.map_or(0, |d| -d), if j.urgent { j.seq as i64 } else { -(j.seq as i64) })
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_capacity_includes_mips_and_the_gpu_budget() {
        let f = Frames::default();
        assert_eq!(rgba8_texture_bytes(2560, 1440, 12), 19_660_652);
        assert_eq!(rgba8_texture_bytes(1, 8, 4), 60);
        assert_eq!(rgba8_texture_bytes(5, 3, 3), 72);
        assert_eq!(rgba8_texture_bytes(0, 10, 32), 0);
        assert!(rgba8_texture_bytes(u32::MAX, u32::MAX, u32::MAX) >= 4);
        assert_eq!(f.preview_capacity(2560, 1440, true), 49);
        assert_eq!(f.preview_capacity(2560, 1440, false), 196);
        f.cache.lock().unwrap().gpu_out_of_memory();
        assert_eq!(f.preview_capacity(2560, 1440, true), 24, "OOM backoff also changes the playback wait range");
        f.set_budget(120);
        assert_eq!(f.preview_capacity(1, 8, true), 1);
        assert_eq!(f.preview_capacity(1, 8, false), 3);
        f.set_budget(0);
        assert_eq!(f.preview_capacity(u32::MAX, u32::MAX, true), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn disk_indicator_does_not_hash_on_ui_thread_and_coalesces_edits() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        struct Disk(u128);
        impl RemoteFrames for Disk {
            fn slots(&self) -> usize {
                1
            }
            fn disk(&self) -> bool {
                true
            }
            fn disk_contains(&self, key: u128) -> bool {
                self.0 == key
            }
            fn start(&self, _: RemoteJob, done: RemoteDone) {
                done(Err("test has no renderer".into()));
            }
        }
        let mut s = effectcraft_engine::Session::default();
        s.execute("comp.new", serde_json::json!({"width":32,"height":18,"duration":1})).unwrap();
        let cid = s.active_comp_id().unwrap();
        let opts = RenderOpts::default();
        let stored = disk_cache::frame_key(disk_cache::comp_content_key(&s.project, cid), 2, 1000, opts_hash(&opts));
        let mut frames = Frames { pool: Some(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()), ..Default::default() };
        frames.set_remote(Some(Arc::new(Disk(stored))));
        let (entered, started) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        frames.pool.as_ref().unwrap().spawn(move || {
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        let src = RenderSource {
            project: s.project.clone(),
            footage: s.footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let mut base = key(1, 0, 0);
        base.comp = cid.0;
        for revision in 1..50 {
            base.revision = revision;
            assert!(frames.disk_frames(&src, &base, 30, &opts).is_empty());
        }
        assert!(frames.content_keys.lock().unwrap().is_empty(), "painting computed a content hash while the worker was blocked");
        assert_eq!(frames.content_key_jobs.lock().unwrap().pending.len(), 1);
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while !frames.content_keys.lock().unwrap().contains_key(&(base.revision, base.comp)) {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(frames.disk_frames(&src, &base, 30, &opts), vec![2], "the blue indicator must return once its key is ready");
        assert_eq!(frames.content_keys.lock().unwrap().len(), 1, "superseded revisions were hashed");
    }

    /// Frames larger than the texture limit are averaged down by a whole factor (#201: an
    /// 11000×2200 comp at Full resolution against an 8192 limit).
    #[test]
    fn frames_over_the_texture_limit_are_averaged_down() {
        assert_eq!(fitted_size([11000, 2200], 8192), [5500, 1100]);
        assert_eq!(fitted_size([8192, 100], 8192), [8192, 100]);
        assert_eq!(fitted_size([30000, 30000], 16384), [15000, 15000]);
        assert_eq!(fitted_size([0, 0], 0), [0, 0]);
        // 5×2 (three columns of white, then black) into 2 px: 3×2 blocks (the last one 2×2).
        let w = egui::Color32::WHITE;
        let b = egui::Color32::from_rgba_premultiplied(0, 0, 0, 255);
        let img = egui::ColorImage::new([5, 2], vec![w, w, w, b, b, w, w, w, b, b]);
        let out = fit_texture(img.clone(), 2);
        assert_eq!(out.size, [2, 1]);
        assert_eq!(out.pixels, vec![w, b]);
        assert_eq!(fit_texture(img.clone(), 5).pixels, img.pixels);
        let mixed = fit_texture(egui::ColorImage::new([2, 1], vec![w, b]), 1);
        assert_eq!(mixed.pixels, vec![egui::Color32::from_rgba_premultiplied(128, 128, 128, 255)]);
    }

    #[test]
    fn retirement_reaches_existing_workers_and_keeps_cpu_frames_and_completion_live() {
        let frames = Frames::default();
        let worker = frames.worker();
        let first = key(1, 0, 0);
        insert(&frames, first);
        frames.retire_gpu();
        assert!(worker.gpu_retired.load(Ordering::Acquire), "already queued workers share retirement");
        assert!(frames.is_cached(&first), "healthy materialized CPU frames survive");
        let next = key(1, 1, 0);
        frames.inflight.lock().unwrap().insert(next);
        frames.queue.lock().unwrap().running.insert(next);
        worker.finish_key(next, false, img(), web_time::Instant::now());
        assert!(frames.is_cached(&next), "CPU retry can still publish");
        assert_eq!(frames.inflight(), 0);
        assert!(frames.queue.lock().unwrap().running.is_empty());
    }

    fn key(content: u64, frame: i64, opts: u64) -> FrameKey {
        FrameKey { revision: content, content, comp: 1, frame, scale: 1000, view: 0, opts }
    }

    fn img() -> FrameImage {
        FrameImage::Cpu(Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLACK; 4])))
    }

    fn insert(f: &Frames, k: FrameKey) {
        f.cache.lock().unwrap().insert(k, img());
    }

    /// A frame whose render panics (a GPU out of memory used to, on every frame thread) is
    /// released, not left "in progress" forever, so the viewer can ask for it again (#106).
    #[test]
    fn a_frame_whose_render_panics_is_released() {
        use effectcraft_engine::Session;
        use effectcraft_engine::project::{Footage, FootageKind};
        use effectcraft_engine::render::Image;
        use serde_json::json;
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Default)]
        struct Explodes(AtomicUsize);
        impl FootageSource for Explodes {
            fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("a decoder bug");
            }
        }
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let clip = Footage { kind: FootageKind::Still, width: 8, height: 8, has_video: true, ..Default::default() };
        let item = Arc::make_mut(&mut s.project).add_item("clip.png", Default::default(), None, ItemKind::Footage(clip));
        s.execute("layer.addItem", json!({"item": item.0})).unwrap();
        let footage = Arc::new(Explodes::default());
        let src = RenderSource {
            project: s.project.clone(),
            footage: footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let f = Frames::default();
        let key = FrameKey { revision: s.revision, content: f.content_of(&s.project, s.revision, cid), ..key(0, 0, 0) };
        let key = FrameKey { comp: cid.0, ..key };
        f.request(&src, key, cid, Tick::ZERO, RenderOpts::default());
        for _ in 0..1000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(footage.0.load(Ordering::SeqCst) > 0, "the render reached the footage");
        assert_eq!(f.inflight(), 0, "released");
        assert!(!f.is_cached(&key));
    }

    #[test]
    fn checked_bloom_preview_failure_is_terminal_uncached_and_repairable() {
        use effectcraft_engine::render::disk_cache::Kind;
        use effectcraft_engine::{Session, keyframe::Value, project::LayerId};
        use serde_json::json;
        let mut session = Session::default();
        let cid = ItemId(session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let layer = session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
        session.execute("effect.apply", json!({"layers": [layer], "effect": "Native Bloom"})).unwrap();
        Arc::make_mut(&mut session.project).comp_mut(cid).unwrap().layer_mut(LayerId(layer)).unwrap().props.prop_mut("effects/#1/radius").unwrap().value =
            Value::Scalar(f64::MAX);
        let directory = std::env::temp_dir().join(format!("ec-checked-bloom-preview-{}", std::process::id()));
        let disk = DiskCache::open(&directory, 1024 * 1024).unwrap();
        let mut source = RenderSource {
            project: session.project.clone(),
            footage: session.footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: Some(disk.clone()),
        };
        // Drive the queue synchronously, avoiding timing-dependent worker assertions.
        let frames = Frames { remote: Some(Arc::new(ParkedRemote)), ..Default::default() };
        let key = FrameKey { comp: cid.0, ..key(1, 0, 0) };
        frames.request_urgent(&source, key, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        assert!(frames.failure(&key).unwrap().contains("ec.stylize.bloom"));
        assert!(!frames.is_cached(&key));
        assert_eq!(frames.inflight(), 0);
        assert!(!frames.urgent_pending());
        assert_eq!(source.layer_cache.stats().entries, 0);
        assert!(!disk.contains(Kind::Frame, frame_disk_key(&frames.content_keys, &source.project, &key, opts_hash(&RenderOpts::default()))));
        for _ in 0..100 {
            frames.request_urgent(&source, key, cid, Tick::ZERO, RenderOpts::default());
        }
        assert!(frames.queue.lock().unwrap().jobs.is_empty(), "repaints cannot loop on a terminal failed frame");
        frames.retry(&key);
        assert!(frames.failure(&key).is_none());
        Arc::make_mut(&mut source.project).comp_mut(cid).unwrap().layer_mut(LayerId(layer)).unwrap().props.prop_mut("effects/#1/radius").unwrap().value =
            Value::Scalar(0.0);
        let repaired = FrameKey { content: 2, revision: 2, ..key };
        frames.request_urgent(&source, repaired, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        assert!(frames.is_cached(&repaired));
        disk.flush();
        drop(source);
        drop(disk);
        drop(frames);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// #329: scrubbing a long movie, the viewer frame of every step the pointer passed started
    /// rendering and seeked the movie's decoder in turn; the frame under the pointer waited for
    /// all of them. A newer viewer frame of another time calls off the one rendering, which is
    /// not cached (its footage may be missing).
    #[test]
    fn a_newer_viewer_frame_calls_off_the_one_rendering() {
        use effectcraft_engine::Session;
        use effectcraft_engine::project::{Footage, FootageKind};
        use effectcraft_engine::render::Image;
        use effectcraft_engine::time::FrameRate;
        use serde_json::json;
        use std::cell::RefCell;
        thread_local! {
            static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
        }
        /// A movie whose first frames take a long seek, which a called-off read gives up.
        #[derive(Default)]
        struct Seeking {
            seeking: AtomicBool,
            called_off: AtomicBool,
        }
        impl FootageSource for Seeking {
            fn cancellable(&self, cancel: &Arc<AtomicBool>, f: &mut dyn FnMut()) {
                let prev = CANCEL.replace(Some(cancel.clone()));
                f();
                CANCEL.set(prev);
            }
            fn frame(&self, _: ItemId, _: &Footage, t: Tick) -> Option<Arc<Image>> {
                if t < Tick::from_seconds_f64(0.25) {
                    self.seeking.store(true, Ordering::SeqCst);
                    for _ in 0..1000 {
                        if CANCEL.with_borrow(|c| c.as_ref().is_some_and(|c| c.load(Ordering::SeqCst))) {
                            self.called_off.store(true, Ordering::SeqCst);
                            return None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                Some(Arc::new(Image::new(4, 4)))
            }
        }
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 10})).unwrap()["comp"].as_u64().unwrap());
        let clip = Footage {
            kind: FootageKind::Video,
            width: 4,
            height: 4,
            has_video: true,
            frame_rate: FrameRate::FPS_30,
            duration: Tick::from_seconds_f64(10.0),
            ..Default::default()
        };
        let item = Arc::make_mut(&mut s.project).add_item("clip.mov", Default::default(), None, ItemKind::Footage(clip));
        s.execute("layer.addItem", json!({"item": item.0})).unwrap();
        let footage = Arc::new(Seeking::default());
        let src = RenderSource {
            project: s.project.clone(),
            footage: footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let f = Frames::default();
        let content = f.content_of(&s.project, s.revision, cid);
        let key = |frame| FrameKey { revision: s.revision, content, comp: cid.0, frame, scale: 1000, view: 0, opts: 0 };
        f.request_urgent(&src, key(0), cid, Tick::ZERO, RenderOpts::default());
        for _ in 0..1000 {
            if footage.seeking.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(footage.seeking.load(Ordering::SeqCst), "the first frame's render reached the footage");
        f.request_urgent(&src, key(150), cid, FrameRate::FPS_30.tick_of(150), RenderOpts::default());
        for _ in 0..1000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(footage.called_off.load(Ordering::SeqCst), "the superseded frame's seek ran on");
        assert!(f.is_cached(&key(150)), "the newest frame rendered");
        assert!(!f.is_cached(&key(0)), "a called-off frame is not cached");
        assert_eq!(f.inflight(), 0, "released");
    }

    /// #284: scrubbing asks for a viewer frame at every step; a newer one drops the comp's viewer
    /// frames still waiting (they would all render first), but not prefetch frames or another
    /// comp's viewer frame.
    #[test]
    fn checked_raster_preview_failure_preserves_the_previous_cached_frame_and_document() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut session = Session::default();
        let cid = ItemId(session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap();
        let original = serde_json::to_vec(&session.project).unwrap();
        let source = RenderSource {
            project: session.project.clone(),
            footage: session.footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let frames = Frames { remote: Some(Arc::new(ParkedRemote)), ..Default::default() };
        let good = FrameKey { comp: cid.0, ..key(1, 0, 0) };
        frames.request_urgent(&source, good, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        let previous = frames.get(&good).unwrap();
        let invalid = FrameKey { opts: 2, ..good };
        frames.request_urgent(
            &source,
            invalid,
            cid,
            Tick::ZERO,
            RenderOpts { roi: Some([0.0, 0.0, f64::from(u32::MAX), f64::from(u32::MAX)]), ..Default::default() },
        );
        assert!(frames.worker().run_next());
        assert!(frames.failure(&invalid).unwrap().contains("raster geometry"));
        assert!(!frames.is_cached(&invalid));
        assert_eq!(frames.inflight(), 0);
        let FrameImage::Cpu(previous) = previous else { panic!("CPU fixture") };
        let FrameImage::Cpu(retained) = frames.get(&good).unwrap() else { panic!("CPU fixture") };
        assert!(Arc::ptr_eq(&previous, &retained), "the last completed frame stays available");
        assert_eq!(serde_json::to_vec(&session.project).unwrap(), original);
        frames.retry(&invalid);
        frames.request_urgent(&source, invalid, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        assert!(frames.is_cached(&invalid));
        assert!(frames.failure(&invalid).is_none());
    }

    #[test]
    fn checked_registered_plugin_preview_retains_completed_frame_document_and_disk_cache() {
        use effectcraft_engine::effects::plugin::{EffectPlugin, PluginFrame, PluginManifest, PluginParam, PluginParamKind, PluginParams, register_plugin};
        use effectcraft_engine::{Session, keyframe::Value, project::LayerId, render::disk_cache::Kind};
        use serde_json::json;
        struct Fault(PluginManifest);
        impl EffectPlugin for Fault {
            fn manifest(&self) -> &PluginManifest {
                &self.0
            }
            fn render(&self, frame: &mut PluginFrame, params: &PluginParams, _time: f64) -> Result<(), String> {
                match params.f("mode") as u32 {
                    1 => {
                        frame.pixels[0] = [0.0; 4];
                        Err("synthetic preview plug-in error".into())
                    }
                    2 => {
                        frame.pixels[0] = [0.0; 4];
                        panic!("synthetic preview plug-in panic")
                    }
                    _ => Ok(()),
                }
            }
        }
        let spec = register_plugin(Arc::new(Fault(PluginManifest {
            api: 1,
            id: "org.test.preview-checked-plugin".into(),
            name: "Checked Preview Plug-in".into(),
            category: "Test".into(),
            version: String::new(),
            author: String::new(),
            description: String::new(),
            params: vec![PluginParam { id: "mode".into(), name: "Mode".into(), kind: PluginParamKind::Angle { default: 0.0 } }],
        })))
        .unwrap();
        let mut session = Session::default();
        let cid = ItemId(session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let lid = session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
        session.execute("effect.apply", json!({"layers": [lid], "effect": spec.id})).unwrap();
        let original = serde_json::to_vec(&session.project).unwrap();
        let directory = std::env::temp_dir().join(format!("ec-checked-plugin-preview-{}", std::process::id()));
        let disk = DiskCache::open(&directory, 1024 * 1024).unwrap();
        let mut source = RenderSource {
            project: session.project.clone(),
            footage: session.footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: Some(disk.clone()),
        };
        let frames = Frames { remote: Some(Arc::new(ParkedRemote)), ..Default::default() };
        let good = FrameKey { comp: cid.0, ..key(1, 0, 0) };
        frames.request_urgent(&source, good, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        let FrameImage::Cpu(previous) = frames.get(&good).unwrap() else { panic!("CPU fixture") };
        let entries = source.layer_cache.stats().entries;
        for (revision, mode, reason) in [(2, 1.0, "synthetic preview plug-in error"), (3, 2.0, "Rust plug-in panicked")] {
            Arc::make_mut(&mut source.project).comp_mut(cid).unwrap().layer_mut(LayerId(lid)).unwrap().props.prop_mut("effects/#1/mode").unwrap().value =
                Value::Scalar(mode);
            let before = serde_json::to_vec(&source.project).unwrap();
            let failed = FrameKey { content: revision, revision, ..good };
            frames.request_urgent(&source, failed, cid, Tick::ZERO, RenderOpts::default());
            assert!(frames.worker().run_next());
            assert!(frames.failure(&failed).unwrap().contains(reason));
            assert!(!frames.is_cached(&failed));
            assert_eq!(frames.inflight(), 0);
            assert_eq!(source.layer_cache.stats().entries, entries);
            assert!(!disk.contains(Kind::Frame, frame_disk_key(&frames.content_keys, &source.project, &failed, opts_hash(&RenderOpts::default()))));
            let FrameImage::Cpu(retained) = frames.get(&good).unwrap() else { panic!("CPU fixture") };
            assert!(Arc::ptr_eq(&previous, &retained));
            assert_eq!(serde_json::to_vec(&source.project).unwrap(), before);
            assert_eq!(serde_json::to_vec(&session.project).unwrap(), original);
            frames.request_urgent(&source, failed, cid, Tick::ZERO, RenderOpts::default());
            assert!(frames.queue.lock().unwrap().jobs.is_empty());
        }
        Arc::make_mut(&mut source.project).comp_mut(cid).unwrap().layer_mut(LayerId(lid)).unwrap().props.prop_mut("effects/#1/mode").unwrap().value =
            Value::Scalar(0.0);
        let repaired = FrameKey { content: 4, revision: 4, ..good };
        frames.request_urgent(&source, repaired, cid, Tick::ZERO, RenderOpts::default());
        assert!(frames.worker().run_next());
        let FrameImage::Cpu(actual) = frames.get(&repaired).unwrap() else { panic!("CPU fixture") };
        assert_eq!(actual.pixels, previous.pixels);
        disk.flush();
        drop(source);
        drop(frames);
        drop(disk);
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct ParkedRemote;
    impl RemoteFrames for ParkedRemote {
        fn slots(&self) -> usize {
            0
        }
        fn start(&self, _job: RemoteJob, _done: RemoteDone) {
            panic!("manual test queue must not dispatch remotely")
        }
    }

    /// Each time the GPU runs out of memory, half as many GPU frames are kept, down to a floor.
    #[test]
    fn running_out_of_video_memory_halves_the_gpu_frame_budget() {
        let f = Frames::default();
        let mut c = f.cache.lock().unwrap();
        assert_eq!(c.gpu_budget, GPU_BUDGET);
        c.gpu_out_of_memory();
        assert_eq!(c.gpu_budget, GPU_BUDGET / 2);
        for _ in 0..20 {
            c.gpu_out_of_memory();
        }
        assert_eq!(c.gpu_budget, MIN_GPU_BUDGET);
    }

    #[test]
    fn least_recently_shown_frames_go_first() {
        let f = Frames::default();
        f.set_budget(3 * 16);
        for i in 0..3 {
            insert(&f, key(1, i, 0));
        }
        // Frame 0 is shown again, so frame 1 is now the oldest.
        assert!(f.get(&key(1, 0, 0)).is_some());
        insert(&f, key(1, 3, 0));
        assert!(f.is_cached(&key(1, 0, 0)) && !f.is_cached(&key(1, 1, 0)));
        assert!(f.is_cached(&key(1, 2, 0)) && f.is_cached(&key(1, 3, 0)));
    }

    fn playhead(cur: i64, looping: bool) -> Playhead {
        Playhead { series: key(1, 0, 0), cur, start: 0, end: 99, step: 1, looping }
    }

    /// #595: a playing preview's cache is full. The frames just ahead of the playhead were
    /// rendered before the ones just shown, so plain LRU evicted them first and the preview
    /// played into a gap. Frames the preview no longer reaches go first, then the ones it
    /// reaches last.
    #[test]
    fn a_full_cache_keeps_the_frames_just_ahead_of_the_playhead() {
        let f = Frames::default();
        f.set_budget(4 * 16);
        // Prefetch rendered 11..=13, then 10 was shown (the most recently used).
        for i in [11, 12, 13, 10] {
            insert(&f, key(1, i, 0));
        }
        let cached = |f: &Frames| f.cached_frames(&key(1, 0, 0));
        f.set_playhead(Some(playhead(10, false)));
        // The next frame rendered is the one the preview reaches last: the frames just ahead stay.
        insert(&f, key(1, 14, 0));
        assert_eq!(cached(&f), [10, 11, 12, 13]);
        // Without looping, a frame behind the playhead goes before any frame ahead of it.
        f.set_playhead(Some(playhead(12, false)));
        insert(&f, key(1, 14, 0));
        assert_eq!(cached(&f), [10, 12, 13, 14]);
        // Looping, the frame just behind is the one reached last.
        f.set_playhead(Some(playhead(13, true)));
        insert(&f, key(1, 0, 0));
        assert_eq!(cached(&f), [0, 10, 13, 14]);
        // Stopped: least recently shown first again.
        f.set_playhead(None);
        insert(&f, key(1, 50, 0));
        assert!(!f.is_cached(&key(1, 13, 0)) && f.is_cached(&key(1, 50, 0)));
    }

    #[test]
    fn playhead_distances_saturate_for_extreme_frame_ranges() {
        let mut position = Playhead { series: key(1, 0, 0), cur: i64::MIN, start: i64::MIN, end: i64::MAX, step: 1, looping: false };
        assert_eq!(position.ahead(&key(1, i64::MAX, 0)), Some(i64::MAX));
        position.cur = i64::MAX;
        position.looping = true;
        assert_eq!(position.ahead(&key(1, 0, 0)), Some(i64::MAX));
        assert_eq!(position.ahead(&key(1, i64::MIN, 0)), Some(1));
        position.step = i64::MIN;
        assert_eq!(position.ahead(&key(1, 0, 0)), Some(i64::MAX));
        assert_eq!(position.ahead(&key(2, 0, 0)), None);
    }

    /// GPU budget pressure must not undo the RAM budget's nearest-frame protection.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn gpu_budget_keeps_next_frames_and_never_evicts_cpu_pixels() {
        let gpu = Gpu::headless();
        assert!(!std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|value| value == "1") || gpu.is_some());
        let Some(gpu) = gpu else { return };
        let image = || {
            let texture = gpu.device().create_texture(&eframe::wgpu::TextureDescriptor {
                label: Some("GPU cache eviction regression"),
                size: eframe::wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: eframe::wgpu::TextureDimension::D2,
                format: eframe::wgpu::TextureFormat::Rgba8Unorm,
                usage: eframe::wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            FrameImage::Gpu(Arc::new(DisplayFrame { texture, width: 2, height: 2 }))
        };
        let mut cache = Cache { map: HashMap::new(), order: VecDeque::new(), bytes: 0, gpu_bytes: 0, budget: 10 * 16, gpu_budget: 4 * 16, playhead: None };
        let cpu_key = key(9, 77, 0);
        cache.insert(cpu_key, FrameImage::Cpu(Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::WHITE))));
        let gpu_frames = |cache: &Cache| {
            let mut frames: Vec<_> = cache.map.iter().filter(|(_, image)| image.is_gpu()).map(|(key, _)| key.frame).collect();
            frames.sort_unstable();
            frames
        };
        // The next needed frame is oldest because prefetch completed before current-frame presentation.
        for frame in [11, 12, 13, 10] {
            cache.insert(key(1, frame, 0), image());
        }
        cache.playhead = Some(playhead(10, false));
        cache.insert(key(1, 14, 0), image());
        assert_eq!(gpu_frames(&cache), [10, 11, 12, 13]);
        assert!(cache.map.contains_key(&cpu_key));
        assert_eq!((cache.gpu_bytes, cache.bytes), (64, 80));
        cache.playhead = Some(playhead(12, false));
        cache.insert(key(1, 14, 0), image());
        assert_eq!(gpu_frames(&cache), [10, 12, 13, 14]);
        cache.playhead = Some(playhead(13, true));
        cache.insert(key(1, 0, 0), image());
        assert_eq!(gpu_frames(&cache), [0, 10, 13, 14]);
        cache.playhead = None;
        cache.insert(key(1, 50, 0), image());
        assert_eq!(gpu_frames(&cache), [0, 10, 14, 50], "paused eviction remains LRU among GPU entries");
        assert!(cache.map.contains_key(&cpu_key));
        // An unrelated GPU series goes before the current series when all of its frames are useful.
        cache.map.clear();
        cache.order.clear();
        cache.bytes = 0;
        cache.gpu_bytes = 0;
        cache.playhead = Some(playhead(0, false));
        for frame in [1, 2, 3, 0] {
            cache.insert(key(1, frame, 0), image());
        }
        cache.insert(key(2, 10, 0), image());
        assert_eq!(gpu_frames(&cache), [0, 1, 2, 3]);
        assert!(!cache.map.contains_key(&key(2, 10, 0)));
        assert_eq!(cache.gpu_bytes, 64);
    }

    /// #595: prefetch renders the frames the preview reaches first, whatever order they were
    /// asked for in (a frame evicted and asked for again waited behind every frame further
    /// ahead); queued frames the preview no longer reaches are dropped and not left in flight.
    #[test]
    fn prefetch_renders_the_frames_the_playhead_reaches_first() {
        struct Idle;
        impl RemoteFrames for Idle {
            fn slots(&self) -> usize {
                0
            }
            fn start(&self, _: RemoteJob, _: RemoteDone) {}
        }
        let mut f = Frames::default();
        f.set_remote(Some(Arc::new(Idle)));
        let src = RenderSource {
            project: Arc::default(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        for frame in [40, 30, 20, 5, 12, 11] {
            f.request(&src, key(1, frame, 0), ItemId(1), Tick::ZERO, RenderOpts::default());
        }
        f.request(&src, FrameKey { comp: 2, ..key(1, 1, 0) }, ItemId(2), Tick::ZERO, RenderOpts::default());
        f.set_playhead(Some(playhead(10, false)));
        let mut order = vec![];
        loop {
            let mut q = f.queue.lock().unwrap();
            let Some(i) = q.best() else { break };
            let j = q.jobs.swap_remove(i);
            order.push((j.key.comp, j.key.frame));
        }
        assert_eq!(order, [(1, 11), (1, 12), (1, 20), (1, 30), (1, 40), (2, 1)]);
        assert_eq!(f.inflight(), 6, "frame 5, behind the playhead, is no longer in flight");
    }

    #[test]
    fn frames_are_keyed_by_content_and_series_are_kept_apart() {
        let f = Frames::default();
        insert(&f, key(1, 0, 0));
        insert(&f, key(2, 0, 0));
        insert(&f, key(2, 1, 7));
        // Other content or render options (Draft, shadows…) are another series.
        assert_eq!(f.cached_frames(&key(2, 0, 0)), vec![0]);
        assert_eq!(f.cached_frames(&key(2, 0, 7)), vec![1]);
        // A later revision with the same content finds the frame (an edit elsewhere, an undo).
        assert!(f.is_cached(&FrameKey { revision: 99, ..key(1, 0, 0) }));
        assert!(!f.is_cached(&key(3, 0, 0)));
    }

    #[test]
    fn live_edits_present_the_newest_completed_revision_without_going_backwards() {
        let f = Frames::default();
        let shown = key(1, 0, 0);
        let newest = key(3, 0, 0);
        let wanted = key(5, 0, 0);
        insert(&f, shown);
        insert(&f, newest);
        insert(&f, key(2, 0, 0)); // Older worker finishes after the newer one.
        let displayed = f.get_for_viewer(&wanted, Some(&shown)).unwrap().0;
        assert_eq!(displayed, newest);
        assert!(f.get_for_viewer(&wanted, Some(&displayed)).is_none(), "never replay an older completion");
        insert(&f, wanted);
        assert_eq!(f.get_for_viewer(&wanted, Some(&displayed)).unwrap().0, wanted, "settle on the exact final edit");
        // Undo reuses the content of an earlier revision immediately.
        let undo = FrameKey { revision: 6, ..shown };
        let reused = f.get_for_viewer(&undo, Some(&wanted)).unwrap().0;
        assert_eq!(reused.content, shown.content);
        assert_eq!(reused.revision, undo.revision);
    }

    #[test]
    fn live_edits_do_not_substitute_other_times_views_or_render_settings() {
        let f = Frames::default();
        let shown = key(1, 0, 0);
        let wanted = key(3, 0, 0);
        let done = key(2, 0, 0);
        for mismatch in [
            FrameKey { comp: 9, ..done },
            FrameKey { frame: 1, ..done },
            FrameKey { scale: 500, ..done },
            FrameKey { view: 1, ..done },
            FrameKey { opts: 1, ..done },
            FrameKey { revision: 4, ..done },
        ] {
            f.clear();
            insert(&f, mismatch);
            assert!(f.get_for_viewer(&wanted, Some(&shown)).is_none(), "wrong candidate: {mismatch:?}");
        }
        f.clear();
        insert(&f, done);
        assert!(f.get_for_viewer(&wanted, None).is_none(), "no stale content on opening a viewer");
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { comp: 9, ..shown })).is_none());
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { frame: 1, ..shown })).is_none());
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { view: 1, ..shown })).is_none());
    }

    #[test]
    fn an_edit_keeps_the_frames_of_comps_it_does_not_touch() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let b = ItemId(s.execute("comp.new", json!({"name": "B", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let f = Frames::default();
        let at = |s: &Session, c: ItemId| f.content_of(&s.project, s.revision, c);
        let (a0, b0) = (at(&s, a), at(&s, b));
        // Edit B: A's content (and frames) stay, B's change.
        s.execute("layer.newSolid", json!({"comp": b.0, "color": "#ff0000"})).unwrap();
        assert_eq!(at(&s, a), a0);
        assert_ne!(at(&s, b), b0);
        // Undo: B is back to the content its old frames were rendered for.
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(at(&s, b), b0);
    }

    #[test]
    fn a_long_drag_keeps_a_bounded_number_of_states() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let f = Frames::default();
        let mut last = None;
        for i in 0..200 {
            s.execute("comp.settings", json!({"width": 8 + i, "merge": "drag"})).unwrap();
            let k = FrameKey { content: f.content_of(&s.project, s.revision, a), ..key(0, 0, 0) };
            insert(&f, k);
            last = Some(k);
        }
        // The snapshots and the frames kept stay bounded (a dropped identity's frames go with it,
        // so a later state that reuses its addresses can't be shown an old frame).
        assert!(f.keepers.lock().unwrap().1.len() <= MAX_KEEPERS);
        assert!(f.cache.lock().unwrap().map.len() <= MAX_KEEPERS);
        assert!(f.is_cached(&last.unwrap()), "the current state's frame stays");
    }

    #[test]
    fn an_identity_keeps_its_comps_so_an_edit_in_place_still_changes_it() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        // A project nothing else holds (no undo history): make_mut would edit the comp in place,
        // at the same address, if the identity hadn't kept a snapshot holding it.
        let mut p = Arc::new((*s.project).clone());
        drop(s);
        let f = Frames::default();
        let before = f.content_of(&p, 1, a);
        let comp = Arc::make_mut(&mut p).comp_mut(a).unwrap();
        comp.width = 16;
        assert_ne!(f.content_of(&p, 2, a), before);
    }

    /// Audio, Lock and Shy (and Hide Shy Layers) don't change pixels: toggling them, in the comp
    /// or a comp it nests, keeps its frames, also when toggled back (#103).
    #[test]
    fn audio_lock_and_shy_switches_keep_the_frames() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let id = |v: serde_json::Value, k: &str| v[k].as_u64().unwrap();
        let inner = ItemId(id(s.execute("comp.new", json!({"name": "Inner", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let solid = id(s.execute("layer.newSolid", json!({"comp": inner.0, "color": "#ff0000"})).unwrap(), "layer");
        let outer = ItemId(id(s.execute("comp.new", json!({"name": "Outer", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let nested = id(s.execute("layer.addItem", json!({"comp": outer.0, "item": inner.0})).unwrap(), "layer");
        let f = Frames::default();
        let at = |s: &Session| f.content_of(&s.project, s.revision, outer);
        let o0 = at(&s);
        let switch = |s: &mut Session, comp: ItemId, layer: u64, sw: &str, v: bool| {
            s.execute("layer.setSwitch", json!({"comp": comp.0, "layers": [layer], "switch": sw, "value": v})).unwrap();
        };
        for (comp, layer, sw) in [(outer, nested, "audio"), (outer, nested, "lock"), (outer, nested, "shy"), (inner, solid, "audio"), (inner, solid, "shy")] {
            // Audio off; Lock and Shy on.
            switch(&mut s, comp, layer, sw, sw != "audio");
            assert_eq!(at(&s), o0, "{sw} on in {comp:?}");
        }
        s.execute("comp.setSwitch", json!({"comp": outer.0, "switch": "hideShy", "value": true})).unwrap();
        assert_eq!(at(&s), o0, "Hide Shy Layers");
        switch(&mut s, outer, nested, "audio", true);
        switch(&mut s, outer, nested, "lock", false);
        assert_eq!(at(&s), o0, "toggled back");
        // Switches that change pixels still do.
        switch(&mut s, inner, solid, "video", false);
        let o1 = at(&s);
        assert_ne!(o1, o0, "Video");
        switch(&mut s, outer, nested, "audio", false);
        assert_eq!(at(&s), o1, "a mute after a real edit keeps that edit's frames");
        switch(&mut s, inner, solid, "video", true);
        assert_ne!(at(&s), o1);
    }

    #[test]
    fn a_waiting_render_never_piles_up_another_frame() {
        use effectcraft_engine::Session;
        use effectcraft_engine::project::{Footage, FootageKind};
        use effectcraft_engine::render::Image;
        use effectcraft_engine::time::FrameRate;
        use serde_json::json;
        use std::cell::Cell;
        use std::sync::atomic::AtomicUsize;
        thread_local! {
            static DEPTH: Cell<usize> = const { Cell::new(0) };
        }
        /// A decoder that works in parallel, as the renderer does: its thread joins the loop and
        /// may wait for the parts other threads took.
        #[derive(Default)]
        struct Parallel {
            calls: AtomicUsize,
            deepest: AtomicUsize,
            active: AtomicUsize,
            peak_active: AtomicUsize,
        }
        impl FootageSource for Parallel {
            fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
                let d = DEPTH.get() + 1;
                DEPTH.set(d);
                self.deepest.fetch_max(d, Ordering::SeqCst);
                self.calls.fetch_add(1, Ordering::SeqCst);
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak_active.fetch_max(active, Ordering::SeqCst);
                (0..64).into_par_iter().for_each(|_| std::thread::sleep(std::time::Duration::from_millis(1)));
                self.active.fetch_sub(1, Ordering::SeqCst);
                DEPTH.set(d - 1);
                Some(Arc::new(Image::new(4, 4)))
            }
        }
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 10})).unwrap()["comp"].as_u64().unwrap());
        let clip = Footage {
            kind: FootageKind::Video,
            width: 4,
            height: 4,
            has_video: true,
            frame_rate: FrameRate::FPS_30,
            duration: Tick::from_seconds_f64(10.0),
            ..Default::default()
        };
        let item = Arc::make_mut(&mut s.project).add_item("clip.mov", Default::default(), None, ItemKind::Footage(clip));
        s.execute("layer.addItem", json!({"item": item.0})).unwrap();
        let footage = Arc::new(Parallel::default());
        let src = RenderSource {
            project: s.project.clone(),
            footage: footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let f = Frames::default();
        let content = f.content_of(&s.project, s.revision, cid);
        let frames = 60;
        for frame in 0..frames {
            let key = FrameKey { revision: s.revision, content, comp: cid.0, frame, scale: 1000, view: 0, opts: 0 };
            f.request(&src, key, cid, FrameRate::FPS_30.tick_of(frame), RenderOpts::default());
        }
        for _ in 0..4000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(f.inflight(), 0, "every queued frame rendered");
        assert!(footage.calls.load(Ordering::SeqCst) >= frames as usize);
        assert_eq!(footage.deepest.load(Ordering::SeqCst), 1, "renders nested on a waiting thread");
        assert_eq!(footage.peak_active.load(Ordering::SeqCst), 1, "whole frames competed for the same workers and decoder");
    }

    #[test]
    fn a_failed_active_frame_releases_the_slot_and_the_newest_viewer_request_wins() {
        use effectcraft_engine::Session;
        use effectcraft_engine::project::{Footage, FootageKind};
        use effectcraft_engine::render::Image;
        use effectcraft_engine::time::FrameRate;
        use serde_json::json;
        use std::sync::mpsc;

        struct PausedFirst {
            first: AtomicBool,
            started: mpsc::Sender<()>,
            resume: Mutex<mpsc::Receiver<()>>,
            calls: Mutex<Vec<Tick>>,
        }
        impl FootageSource for PausedFirst {
            fn frame(&self, _: ItemId, _: &Footage, t: Tick) -> Option<Arc<Image>> {
                self.calls.lock().unwrap().push(t);
                if self.first.swap(false, Ordering::AcqRel) {
                    self.started.send(()).unwrap();
                    self.resume.lock().unwrap().recv().unwrap();
                    panic!("synthetic decoder panic after other requests were queued");
                }
                Some(Arc::new(Image::new(4, 4)))
            }
        }
        let mut session = Session::default();
        let cid = ItemId(session.execute("comp.new", json!({"width": 8, "height": 8, "duration": 10})).unwrap()["comp"].as_u64().unwrap());
        let footage = Footage {
            kind: FootageKind::Video,
            width: 4,
            height: 4,
            has_video: true,
            frame_rate: FrameRate::FPS_30,
            duration: Tick::from_seconds_f64(10.0),
            ..Default::default()
        };
        let item = Arc::make_mut(&mut session.project).add_item("synthetic clip", Default::default(), None, ItemKind::Footage(footage));
        session.execute("layer.addItem", json!({"item": item.0})).unwrap();
        let original = serde_json::to_vec(&session.project).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let footage = Arc::new(PausedFirst { first: AtomicBool::new(true), started: started_tx, resume: Mutex::new(resume_rx), calls: Mutex::default() });
        let source = RenderSource {
            project: session.project.clone(),
            footage: footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        // Requests stay queued so the active render and priority handoff are deterministic.
        let frames = Frames { remote: Some(Arc::new(ParkedRemote)), ..Default::default() };
        let frame_key = |frame| FrameKey { comp: cid.0, ..key(1, frame, 0) };
        let request = |frame, urgent| frames.request_with(&source, frame_key(frame), cid, FrameRate::FPS_30.tick_of(frame), RenderOpts::default(), urgent);
        request(0, true);
        let worker = frames.worker();
        std::thread::scope(|scope| {
            let active = scope.spawn(|| worker.run_next());
            let started = started_rx.recv_timeout(std::time::Duration::from_secs(10));
            request(1, false);
            request(2, false);
            request(10, true);
            request(11, true);
            let second_started = started.is_ok() && frames.worker().run_next();
            let calls = footage.calls.lock().unwrap().clone();
            resume_tx.send(()).unwrap();
            let completed = active.join();
            started.unwrap();
            assert!(!second_started, "no second whole frame while the decoder is active");
            assert_eq!(calls.as_slice(), &[Tick::ZERO]);
            assert!(completed.unwrap());
        });
        assert!(frames.failure(&frame_key(0)).is_some());
        assert!(!frames.is_cached(&frame_key(0)));
        assert!(!frames.active_frame.load(Ordering::Acquire));
        assert!(frames.queue.lock().unwrap().running.is_empty());
        for expected in [11, 1, 2] {
            assert!(worker.run_next());
            assert!(frames.is_cached(&frame_key(expected)));
            assert_eq!(*footage.calls.lock().unwrap().last().unwrap(), FrameRate::FPS_30.tick_of(expected));
        }
        assert!(!worker.run_next());
        assert_eq!(frames.inflight(), 0);
        assert!(!frames.urgent_pending());
        assert!(!frames.is_cached(&frame_key(10)), "superseded urgent frame was dropped");
        assert_eq!(serde_json::to_vec(&session.project).unwrap(), original);
        let escaped = std::panic::catch_unwind(|| {
            frames.active_frame.store(true, Ordering::Release);
            let _active = ActiveFrame(&frames.active_frame);
            panic!("synthetic escaped worker panic");
        });
        assert!(escaped.is_err());
        assert!(!frames.active_frame.load(Ordering::Acquire));
    }

    #[test]
    fn a_newer_viewer_frame_drops_the_ones_still_waiting() {
        /// Starts nothing, so every request stays queued.
        struct Idle;
        impl RemoteFrames for Idle {
            fn slots(&self) -> usize {
                0
            }
            fn start(&self, _: RemoteJob, _: RemoteDone) {}
        }
        let mut f = Frames::default();
        f.set_remote(Some(Arc::new(Idle)));
        let src = RenderSource {
            project: Arc::default(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let request = |k: FrameKey, urgent: bool| f.request_with(&src, k, ItemId(k.comp), Tick::ZERO, RenderOpts::default(), urgent);
        let other = FrameKey { comp: 2, ..key(1, 0, 0) };
        request(other, true);
        request(key(1, 20, 0), false);
        for frame in 0..10 {
            request(key(1, frame, 0), true);
        }
        let mut queued: Vec<(u64, i64, bool)> = f.queue.lock().unwrap().jobs.iter().map(|j| (j.key.comp, j.key.frame, j.urgent)).collect();
        queued.sort();
        assert_eq!(queued, [(1, 9, true), (1, 20, false), (2, 0, false)]);
        assert_eq!(f.inflight(), 3, "dropped frames aren't in flight: the viewer can ask again");
    }

    #[test]
    fn markers_keep_the_frames() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let id = |v: serde_json::Value, k: &str| v[k].as_u64().unwrap();
        let outer = ItemId(id(s.execute("comp.new", json!({"name": "Outer", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let solid = id(s.execute("layer.newSolid", json!({"comp": outer.0, "color": "#ff0000"})).unwrap(), "layer");
        let f = Frames::default();
        let at = |s: &Session| f.content_of(&s.project, s.revision, outer);
        let o0 = at(&s);
        s.execute("layer.addMarker", json!({"comp": outer.0, "layers": [solid], "time": 0.5, "comment": "beat"})).unwrap();
        assert_eq!(at(&s), o0, "a marker changes no pixels, so it keeps the frames");
    }

    #[test]
    fn markers_count_when_an_expression_can_read_them() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let id = |v: serde_json::Value, k: &str| v[k].as_u64().unwrap();
        let outer = ItemId(id(s.execute("comp.new", json!({"name": "Outer", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let solid = id(s.execute("layer.newSolid", json!({"comp": outer.0, "color": "#ff0000"})).unwrap(), "layer");
        s.execute("prop.setExpression", json!({"comp": outer.0, "layer": solid, "path": "transform/opacity", "expression": "1"})).unwrap();
        let f = Frames::default();
        let at = |s: &Session| f.content_of(&s.project, s.revision, outer);
        let o0 = at(&s);
        s.execute("layer.addMarker", json!({"comp": outer.0, "layers": [solid], "time": 0.5, "comment": "beat"})).unwrap();
        assert_ne!(at(&s), o0, "an expression can read the marker, so the frames change");
    }
}
