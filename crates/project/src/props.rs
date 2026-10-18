//! The property tree: groups and properties with stable uids and match ids, addressed by paths.

use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_time::Tick;
use serde::{Deserialize, Serialize};

pub type Uid = u64;

/// A layer's property tree shared by immutable project snapshots. Mutating a tree detaches
/// only that layer's properties; composition clones retain all other trees without copying.
/// The transparent representation preserves the existing project JSON format.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PropertyTree(std::sync::Arc<PropGroup>);

impl PropertyTree {
    /// Weak process-local identity for prepared render data. An edit detaches this storage,
    /// invalidating caches without retaining old project trees or scanning their keyframes.
    pub fn downgrade(&self) -> std::sync::Weak<PropGroup> {
        std::sync::Arc::downgrade(&self.0)
    }

    /// Reuse equal properties after decoding an independently serialized revision. Identity
    /// only selects candidates: every field must match before storage is shared. Oversized
    /// or deeply nested trees keep their decoded data; this optional optimization is bounded.
    pub fn share_unchanged_properties(&mut self, previous: &Self) {
        if std::sync::Arc::ptr_eq(&self.0, &previous.0) {
            return;
        }
        const MAX_NODES: usize = 65_536;
        const MAX_DEPTH: usize = 128;
        let mut properties = std::collections::HashMap::new();
        let mut pending = vec![previous.children.iter()];
        let mut visited = 0usize;
        while let Some(children) = pending.last_mut() {
            match children.next() {
                Some(node) => {
                    if visited >= MAX_NODES {
                        return;
                    }
                    visited += 1;
                    match node {
                        Node::Prop(property) => {
                            properties.entry(property.uid).or_insert(property);
                        }
                        Node::Group(group) => {
                            if pending.len() >= MAX_DEPTH {
                                return;
                            }
                            pending.push(group.children.iter());
                        }
                    }
                }
                None => {
                    pending.pop();
                }
            }
        }
        let mut pending = vec![std::sync::Arc::make_mut(&mut self.0).children.iter_mut()];
        let mut visited = 0usize;
        while let Some(children) = pending.last_mut() {
            match children.next() {
                Some(node) => {
                    if visited >= MAX_NODES {
                        return;
                    }
                    visited += 1;
                    match node {
                        Node::Prop(property) => {
                            if let Some(old) = properties.get(&property.uid)
                                && &*property == *old
                                && property.same_serialized_data(old)
                            {
                                *property = (*old).clone();
                            }
                        }
                        Node::Group(group) => {
                            if pending.len() >= MAX_DEPTH {
                                return;
                            }
                            pending.push(group.children.iter_mut());
                        }
                    }
                }
                None => {
                    pending.pop();
                }
            }
        }
    }
}

impl From<PropGroup> for PropertyTree {
    fn from(group: PropGroup) -> Self {
        Self(std::sync::Arc::new(group))
    }
}

impl std::ops::Deref for PropertyTree {
    type Target = PropGroup;

    fn deref(&self) -> &PropGroup {
        &self.0
    }
}

impl std::ops::DerefMut for PropertyTree {
    fn deref_mut(&mut self) -> &mut PropGroup {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl PartialEq for PropertyTree {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

/// How a property is presented and edited.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ParamUi {
    /// Plain number with an optional valid range and a (narrower) slider range.
    #[default]
    Number,
    Slider {
        min: f64,
        max: f64,
        slider_min: f64,
        slider_max: f64,
        decimals: u8,
    },
    Percent,
    /// Angle in degrees, shown as `revolutions x +degrees`.
    Angle,
    Pixels,
    Point,
    Point3,
    Color,
    Checkbox,
    Popup {
        options: Vec<String>,
    },
    Layer,
    /// A mask of the effect's own layer by 1-based index, 0 = none (a scalar value): an effect's
    /// "Path" popup.
    Mask,
    Path,
    Text,
    /// A file path (a string value) chosen with a "Choose…" button, offering files with these
    /// extensions: Apply Color LUT's LUT, an OCIO config.
    File {
        filters: Vec<String>,
    },
    Gradient,
    /// Not shown in Effect Controls / the timeline (internal data).
    Hidden,
}

/// Expression attached to a property.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Expression {
    pub text: String,
    pub enabled: bool,
}

/// Immutable snapshots share each property's data too. Editing another property in the same
/// tree must not duplicate this property's keyframes, text, paths or expression.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Property(std::sync::Arc<PropertyData>);

/// Scratch space for exact storage comparison. An unavailable optimization must not allocate
/// an unbounded second copy of a hostile property's serialized data.
#[derive(Default)]
struct PropertyBytes(Vec<u8>);

impl std::io::Write for PropertyBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        const LIMIT: usize = 8 * 1024 * 1024;
        if bytes.len() > LIMIT.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("property sharing comparison exceeds its byte limit"));
        }
        self.0.try_reserve(bytes.len()).map_err(|_| std::io::Error::other("property sharing scratch allocation unavailable"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The editable property fields. The transparent owner preserves the existing project format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropertyData {
    pub uid: Uid,
    /// Stable id within its parent kind (`position`, `blurriness`…).
    #[serde(rename = "match")]
    pub match_id: String,
    pub name: String,
    /// Static value (used when there are no keyframes).
    pub value: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<Expression>,
    #[serde(default)]
    pub ui: ParamUi,
    /// Motion-path semantics (Position, Anchor Point, points).
    #[serde(default)]
    pub spatial: bool,
    /// Only hold keyframes (Source Text, checkboxes, popups).
    #[serde(default)]
    pub hold_only: bool,
    /// Valid only on 3D layers (Orientation, X/Y Rotation…).
    #[serde(default)]
    pub three_d_only: bool,
    /// Valid only on 2D layers.
    #[serde(default)]
    pub two_d_only: bool,
    /// Dimensions shown/edited (2 for 2D position etc.); 0 = all.
    #[serde(default)]
    pub shown_dims: u8,
    /// Can't be keyframed (header-like values, e.g. Mask Mode). Shown without a stopwatch.
    #[serde(default)]
    pub static_only: bool,
}

impl std::ops::Deref for Property {
    type Target = PropertyData;

    fn deref(&self) -> &PropertyData {
        &self.0
    }
}

impl std::ops::DerefMut for Property {
    fn deref_mut(&mut self) -> &mut PropertyData {
        std::sync::Arc::make_mut(&mut self.0)
    }
}

impl PartialEq for Property {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl Property {
    fn same_serialized_data(&self, other: &Self) -> bool {
        if std::sync::Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        // Numeric PartialEq intentionally equates signed zero. Sharing decoded revisions
        // must also preserve the exact stored representation, including values inside keys,
        // paths, text style and UI metadata. A failed bounded comparison simply keeps both.
        let mut left = PropertyBytes::default();
        let mut right = PropertyBytes::default();
        serde_json::to_writer(&mut left, self).is_ok() && serde_json::to_writer(&mut right, other).is_ok() && left.0 == right.0
    }

    /// Consume the owner, moving unique data or cloning when another snapshot still uses it.
    pub fn into_data(self) -> PropertyData {
        std::sync::Arc::unwrap_or_clone(self.0)
    }

    pub fn new(uid: Uid, match_id: &str, name: &str, value: Value) -> Property {
        let hold_only = !value.interpolates();
        Self(std::sync::Arc::new(PropertyData {
            uid,
            match_id: match_id.into(),
            name: name.into(),
            value,
            keys: vec![],
            expr: None,
            ui: ParamUi::Number,
            spatial: false,
            hold_only,
            three_d_only: false,
            two_d_only: false,
            shown_dims: 0,
            static_only: false,
        }))
    }
    pub fn with_ui(mut self, ui: ParamUi) -> Property {
        self.ui = ui;
        self
    }
    pub fn spatial(mut self) -> Property {
        self.spatial = true;
        self
    }
    pub fn is_animated(&self) -> bool {
        !self.keys.is_empty()
    }
    pub fn has_expression(&self) -> bool {
        self.expr.as_ref().is_some_and(|e| e.enabled && !e.text.trim().is_empty())
    }
    /// Keyframed value at `t` (no expression).
    pub fn value_at(&self, t: Tick) -> Value {
        if self.keys.is_empty() {
            return self.value.clone();
        }
        effectcraft_keyframe::evaluate(&self.keys, t, self.spatial).unwrap_or_else(|| self.value.clone())
    }
    /// Set the value at `t`: adds/replaces a key when animated, else sets the static value.
    pub fn set_value_at(&mut self, t: Tick, v: Value) {
        if self.keys.is_empty() {
            self.value = v;
        } else {
            let mut k = Keyframe::new(t, v);
            if self.hold_only {
                k = k.hold();
            }
            effectcraft_keyframe::set_key(&mut self.keys, k);
        }
    }
    /// Toggle the stopwatch: on → one key at `t` with the current value; off → keep value at `t`.
    pub fn set_animated(&mut self, on: bool, t: Tick) {
        if on && self.keys.is_empty() {
            let mut k = Keyframe::new(t, self.value.clone());
            if self.hold_only {
                k = k.hold();
            }
            self.keys.push(k);
        } else if !on && !self.keys.is_empty() {
            self.value = self.value_at(t);
            self.keys.clear();
        }
    }
}

/// What a group represents, with its non-animatable header data.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum GroupKind {
    #[default]
    Plain,
    /// An instance in an indexed list (masks, effects, shape contents, animators): these can be
    /// renamed, reordered, duplicated and deleted.
    Indexed,
    Mask {
        mode: MaskMode,
        inverted: bool,
        color: [u8; 3],
        locked: bool,
        /// Layer ▸ Mask ▸ Motion Blur.
        #[serde(default)]
        motion_blur: MaskMotionBlur,
        /// Layer ▸ Mask ▸ Feather Falloff.
        #[serde(default)]
        feather_falloff: FeatherFalloff,
        /// Layer ▸ Mask and Shape Path ▸ RotoBezier: tangents follow the vertices automatically.
        #[serde(default)]
        roto_bezier: bool,
    },
    Effect {
        /// Effect spec id, e.g. `ec.blur.gaussian`.
        effect: String,
    },
    /// A motion tracker (Motion Trackers ▸ Tracker n) with its Tracker panel settings.
    Tracker { settings: Box<crate::tracking::TrackerSettings> },
    /// The Essential Properties of a precomp layer (master properties): one child per control
    /// of the nested comp's Essential Graphics; `overridden` lists the children (uids) whose
    /// values this instance overrides.
    Essential {
        #[serde(default)]
        overridden: Vec<Uid>,
    },
}

/// Mask motion blur (sub-frame samples of an animated mask path).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskMotionBlur {
    /// Blur when the layer's Motion Blur switch is on.
    #[default]
    SameAsLayer,
    On,
    Off,
}

/// How a mask's feather ramps: Smooth (Gaussian) or Linear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatherFalloff {
    #[default]
    Smooth,
    Linear,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskMode {
    None,
    #[default]
    Add,
    Subtract,
    Intersect,
    Lighten,
    Darken,
    Difference,
}

impl MaskMode {
    pub const ALL: [MaskMode; 7] =
        [MaskMode::None, MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect, MaskMode::Lighten, MaskMode::Darken, MaskMode::Difference];
    pub fn label(self) -> &'static str {
        match self {
            MaskMode::None => "None",
            MaskMode::Add => "Add",
            MaskMode::Subtract => "Subtract",
            MaskMode::Intersect => "Intersect",
            MaskMode::Lighten => "Lighten",
            MaskMode::Darken => "Darken",
            MaskMode::Difference => "Difference",
        }
    }
    pub fn from_name(s: &str) -> Option<MaskMode> {
        MaskMode::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropGroup {
    pub uid: Uid,
    #[serde(rename = "match")]
    pub match_id: String,
    pub name: String,
    #[serde(default)]
    pub kind: GroupKind,
    /// fx switch for effects, eye for shape groups, etc.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// An effect's colour label (After Effects 26.5), shown in Effect Controls and the Timeline;
    /// `None` is unlabelled. Absent in older projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<effectcraft_color::Label>,
    #[serde(default)]
    pub children: Vec<Node>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node")]
pub enum Node {
    Prop(Property),
    Group(PropGroup),
}

impl Node {
    pub fn uid(&self) -> Uid {
        match self {
            Node::Prop(p) => p.uid,
            Node::Group(g) => g.uid,
        }
    }
    pub fn match_id(&self) -> &str {
        match self {
            Node::Prop(p) => &p.match_id,
            Node::Group(g) => &g.match_id,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Node::Prop(p) => &p.name,
            Node::Group(g) => &g.name,
        }
    }
    pub fn as_prop(&self) -> Option<&Property> {
        if let Node::Prop(p) = self { Some(p) } else { None }
    }
    pub fn as_group(&self) -> Option<&PropGroup> {
        if let Node::Group(g) = self { Some(g) } else { None }
    }
    pub fn as_group_mut(&mut self) -> Option<&mut PropGroup> {
        if let Node::Group(g) = self { Some(g) } else { None }
    }
}

/// One step of a property path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Uid(Uid),
    /// Match id and 1-based occurrence among siblings with that match id.
    Match(String, usize),
    /// 1-based child index.
    Index(usize),
}

/// Parse `transform/position`, `effects/#1/blurriness`, `effects/@57/blurriness`,
/// `contents/rect#2/size`.
/// `@57` (a whole path that is one uid).
fn bare_uid(path: &str) -> Option<Uid> {
    path.trim().strip_prefix('@')?.parse().ok()
}

pub fn parse_path(s: &str) -> Vec<Seg> {
    s.split(['/', '.'])
        .filter(|x| !x.is_empty())
        .map(|seg| {
            if let Some(u) = seg.strip_prefix('@').and_then(|u| u.parse().ok()) {
                Seg::Uid(u)
            } else if let Some(i) = seg.strip_prefix('#').and_then(|u| u.parse().ok()) {
                Seg::Index(i)
            } else if let Some((m, n)) = seg.rsplit_once('#')
                && let Ok(n) = n.parse()
            {
                Seg::Match(m.to_string(), n)
            } else {
                Seg::Match(seg.to_string(), 1)
            }
        })
        .collect()
}

impl PropGroup {
    /// An instance in an indexed list (effect, mask, shape item, text animator, tracker): these
    /// can be renamed, removed, duplicated and reordered; fixed groups (Transform…) can't.
    pub fn is_instance(&self) -> bool {
        matches!(self.kind, GroupKind::Indexed | GroupKind::Effect { .. } | GroupKind::Mask { .. } | GroupKind::Tracker { .. })
    }

    pub fn new(uid: Uid, match_id: &str, name: &str) -> PropGroup {
        PropGroup { uid, match_id: match_id.into(), name: name.into(), kind: GroupKind::Plain, enabled: true, label: None, children: vec![] }
    }
    pub fn with(mut self, n: impl Into<Node>) -> PropGroup {
        self.children.push(n.into());
        self
    }

    fn child_index(&self, seg: &Seg) -> Option<usize> {
        match seg {
            Seg::Uid(u) => self.children.iter().position(|c| c.uid() == *u),
            Seg::Index(i) => (*i >= 1 && *i <= self.children.len()).then(|| i - 1),
            Seg::Match(m, n) => {
                self.children.iter().enumerate().filter(|(_, c)| c.match_id() == m || c.name().eq_ignore_ascii_case(m)).nth(n.saturating_sub(1)).map(|(i, _)| i)
            }
        }
    }

    pub fn node(&self, path: &str) -> Option<&Node> {
        let segs = parse_path(path);
        let (last, init) = segs.split_last()?;
        let mut g = self;
        for s in init {
            g = g.children.get(g.child_index(s)?)?.as_group()?;
        }
        g.children.get(g.child_index(last)?)
    }
    pub fn node_mut(&mut self, path: &str) -> Option<&mut Node> {
        let segs = parse_path(path);
        let (last, init) = segs.split_last()?;
        let mut g = self;
        for s in init {
            let i = g.child_index(s)?;
            g = g.children.get_mut(i)?.as_group_mut()?;
        }
        let i = g.child_index(last)?;
        g.children.get_mut(i)
    }
    /// A property by path; a bare `@uid` finds the property anywhere below this group.
    pub fn prop(&self, path: &str) -> Option<&Property> {
        if let Some(u) = bare_uid(path) {
            return self.find(u);
        }
        self.node(path)?.as_prop()
    }
    pub fn prop_mut(&mut self, path: &str) -> Option<&mut Property> {
        if let Some(u) = bare_uid(path) {
            return self.find_mut(u);
        }
        match self.node_mut(path)? {
            Node::Prop(p) => Some(p),
            _ => None,
        }
    }
    pub fn group(&self, path: &str) -> Option<&PropGroup> {
        self.node(path)?.as_group()
    }
    pub fn group_mut(&mut self, path: &str) -> Option<&mut PropGroup> {
        self.node_mut(path)?.as_group_mut()
    }
    /// Direct child property by match id.
    pub fn get(&self, match_id: &str) -> Option<&Property> {
        self.children.iter().find_map(|c| c.as_prop().filter(|p| p.match_id == match_id))
    }
    pub fn get_mut(&mut self, match_id: &str) -> Option<&mut Property> {
        self.children.iter_mut().find_map(|c| match c {
            Node::Prop(p) if p.match_id == match_id => Some(p),
            _ => None,
        })
    }
    pub fn sub(&self, match_id: &str) -> Option<&PropGroup> {
        self.children.iter().find_map(|c| c.as_group().filter(|g| g.match_id == match_id))
    }
    pub fn sub_mut(&mut self, match_id: &str) -> Option<&mut PropGroup> {
        self.children.iter_mut().find_map(|c| match c {
            Node::Group(g) if g.match_id == match_id => Some(g),
            _ => None,
        })
    }
    pub fn groups(&self) -> impl Iterator<Item = &PropGroup> {
        self.children.iter().filter_map(Node::as_group)
    }
    pub fn props(&self) -> impl Iterator<Item = &Property> {
        self.children.iter().filter_map(Node::as_prop)
    }

    /// Visit every property depth-first with its uid-path (`@g/@g/@p`).
    pub fn walk<'a>(&'a self, prefix: &str, f: &mut dyn FnMut(&str, &'a Property)) {
        for c in &self.children {
            let p = format!("{prefix}@{}", c.uid());
            match c {
                Node::Prop(pr) => f(&p, pr),
                Node::Group(g) => g.walk(&format!("{p}/"), f),
            }
        }
    }
    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Property)) {
        for c in &mut self.children {
            match c {
                Node::Prop(p) => f(p),
                Node::Group(g) => g.walk_mut(f),
            }
        }
    }
    /// Uid path (`@1/@2/@3`) of the node with `uid`.
    pub fn path_of(&self, uid: Uid) -> Option<String> {
        for c in &self.children {
            if c.uid() == uid {
                return Some(format!("@{uid}"));
            }
            if let Node::Group(g) = c
                && let Some(p) = g.path_of(uid)
            {
                return Some(format!("@{}/{p}", g.uid));
            }
        }
        None
    }
    /// Match-id path (`transform/position`, `effects/ec.blur.gaussian#2/blurriness`) of the node
    /// with `uid`: portable between layers with the same structure (keyframe paste, pick-whip).
    pub fn match_path_of(&self, uid: Uid) -> Option<String> {
        for (i, c) in self.children.iter().enumerate() {
            let n = self.children[..i].iter().filter(|o| o.match_id() == c.match_id()).count() + 1;
            let seg = if n == 1 { c.match_id().to_string() } else { format!("{}#{n}", c.match_id()) };
            if c.uid() == uid {
                return Some(seg);
            }
            if let Node::Group(g) = c
                && let Some(p) = g.match_path_of(uid)
            {
                return Some(format!("{seg}/{p}"));
            }
        }
        None
    }
    /// Chain of nodes from this group's children down to `uid` (inclusive).
    pub fn node_chain(&self, uid: Uid) -> Option<Vec<&Node>> {
        for c in &self.children {
            if c.uid() == uid {
                return Some(vec![c]);
            }
            if let Node::Group(g) = c
                && let Some(mut rest) = g.node_chain(uid)
            {
                rest.insert(0, c);
                return Some(rest);
            }
        }
        None
    }
    /// Human path (`Transform/Position`) of the node with `uid`.
    pub fn name_path_of(&self, uid: Uid) -> Option<String> {
        for c in &self.children {
            if c.uid() == uid {
                return Some(c.name().to_string());
            }
            if let Node::Group(g) = c
                && let Some(p) = g.name_path_of(uid)
            {
                return Some(format!("{}/{p}", g.name));
            }
        }
        None
    }
    /// Find a property anywhere in the tree by uid.
    pub fn find(&self, uid: Uid) -> Option<&Property> {
        for c in &self.children {
            match c {
                Node::Prop(p) if p.uid == uid => return Some(p),
                Node::Group(g) => {
                    if let Some(p) = g.find(uid) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }
    pub fn find_mut(&mut self, uid: Uid) -> Option<&mut Property> {
        for c in &mut self.children {
            match c {
                Node::Prop(p) if p.uid == uid => return Some(p),
                Node::Group(g) => {
                    if let Some(p) = g.find_mut(uid) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }
    pub fn find_group(&self, uid: Uid) -> Option<&PropGroup> {
        if self.uid == uid {
            return Some(self);
        }
        self.groups().find_map(|g| g.find_group(uid))
    }
    pub fn find_group_mut(&mut self, uid: Uid) -> Option<&mut PropGroup> {
        if self.uid == uid {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| match c {
            Node::Group(g) => g.find_group_mut(uid),
            _ => None,
        })
    }
    /// Parent group of the node with `uid`.
    pub fn parent_of(&self, uid: Uid) -> Option<&PropGroup> {
        if self.children.iter().any(|c| c.uid() == uid) {
            return Some(self);
        }
        self.groups().find_map(|g| g.parent_of(uid))
    }
    /// Parent group of the node with `uid`.
    pub fn parent_of_mut(&mut self, uid: Uid) -> Option<&mut PropGroup> {
        if self.children.iter().any(|c| c.uid() == uid) {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| match c {
            Node::Group(g) => g.parent_of_mut(uid),
            _ => None,
        })
    }
    /// Any property in this subtree animated (keys or expression).
    pub fn any_animated(&self) -> bool {
        self.children.iter().any(|c| match c {
            Node::Prop(p) => p.is_animated() || p.has_expression(),
            Node::Group(g) => g.any_animated(),
        })
    }
    /// Re-assign fresh uids (after duplicating a subtree).
    pub fn reassign_uids(&mut self, next: &mut Uid) {
        *next += 1;
        self.uid = *next;
        for c in &mut self.children {
            match c {
                Node::Prop(p) => {
                    *next += 1;
                    p.uid = *next;
                }
                Node::Group(g) => g.reassign_uids(next),
            }
        }
    }
    pub fn max_uid(&self) -> Uid {
        self.children
            .iter()
            .map(|c| match c {
                Node::Prop(p) => p.uid,
                Node::Group(g) => g.max_uid(),
            })
            .fold(self.uid, Uid::max)
    }
}

impl From<Property> for Node {
    fn from(p: Property) -> Node {
        Node::Prop(p)
    }
}
impl From<PropGroup> for Node {
    fn from(g: PropGroup) -> Node {
        Node::Group(g)
    }
}
