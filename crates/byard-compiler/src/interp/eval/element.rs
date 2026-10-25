//! Lowering an element (RFC-0005).
//!
//! `lower_element` turns one element node into a `RenderNode`: it expands the
//! `..style` spreads, validates the call against the intrinsic's contract,
//! and builds the node for the intrinsic's family (text, boxes, canvas,
//! navigation, overlays, and the rest), lowering the children and routing
//! the structural members (`when`, `for`) to their pools. The `ClipPath` mask
//! a box may carry is split out of its members and lowered here too.

use super::canvas::{
    CachedMesh, PathCommand, lower_canvas_items, path_fingerprint, tessellate_path,
};
use super::layout::{ForPool, WhenPool};
use super::states::attrs_with_states;
use super::{Interpreter, RESERVED_CONTENT, RenderNode, controller, members_span};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::interp::intrinsics::validate_element;
use crate::parser::ast::{ElementNode, Expr, Member};
use crate::symbol::Symbol;

impl Interpreter {
    /// Lowers an element to a [`RenderNode`], validating it against the §5
    /// attribute contract first (diagnostics accumulate in [`Interpreter::errors`]).
    /// `known_views` are user `ViewDecl` names in scope.
    pub fn lower_element(&mut self, el: &ElementNode, known_views: &[&str]) -> RenderNode {
        // RFC-0016: expand `..style` spreads into a flat attribute set *before*
        // validating or lowering, so everything downstream sees ordinary
        // resolved attributes (and a spread can never leak into the checker).
        let (attrs, state_blocks) = self.expand_style_spreads(&el.attrs);
        // Validate the base *and* every state block's attributes against the
        // intrinsic's contract (an `on hover { bg: … }` must obey the same §5
        // rules as an inline `bg:`); the state attrs are validation-only and do
        // not affect the emitted base set.
        self.check_breakpoints(&state_blocks);
        let to_validate = attrs_with_states(&attrs, &state_blocks);
        self.errors
            .extend(validate_element(el, &to_validate, known_views));
        // RFC-0038: an `on measure` member is *about* this element, so it is
        // consumed here and never reaches the child list. `el_children` is the
        // element's members minus those declarations, borrowed unchanged in the
        // overwhelming case where there are none.
        let (measure, el_children) = self.lower_measures(el, &attrs);
        let el_children: &[Member] = &el_children;
        match el.name.as_str() {
            "Text" | "Button" if !el.content.is_empty() => {
                let content = self.bind_value(&el.content[0].value);
                if el.name.as_str() == "Button" {
                    // A Button is a decorated box wrapping its label, and the
                    // label is where every typographic property has to land.
                    // The button's box cannot use `size`, `weight`, `font` or
                    // `color` for anything, so leaving them on it made all
                    // four accepted, type-checked and completely inert: a
                    // `Button` set in bold rendered at the theme default with
                    // nothing to say so.
                    let label_attrs = Self::text_shaping_attrs(&attrs);
                    RenderNode::Box {
                        name: Symbol::intern("Button"),
                        attrs,
                        state_blocks,
                        anchor_name: el.anchor_name.clone(),
                        children: vec![RenderNode::Text {
                            attrs: label_attrs,
                            state_blocks: Vec::new(),
                            content,
                        }],
                        action: el.action.clone(),
                        bound_sig: None,
                        env_snapshot: self.capture_env_snapshot(),
                        measure,
                        clip_path: None,
                    }
                } else {
                    RenderNode::Text {
                        attrs,
                        state_blocks,
                        content,
                    }
                }
            }
            // RFC-0026: a navigation container. Its children are `route`/`tab`
            // cases, not elements, so it never takes the generic container path
            //, it compiles its route table into a pool here and lowers each
            // screen lazily, the first time navigation reaches it.
            "NavStack" | "NavHost" => self.lower_nav(el, &attrs, state_blocks, known_views),
            "Spacer" => RenderNode::Spacer { attrs },
            // Image intrinsic → TextureSampler pipeline.
            // Syntax: Image("path.jpg") #[fit: .cover, width: 200, height: 150]
            "Image" => {
                let src_expr = el.content.first().map_or_else(
                    || Expr::StrLit(vec![], crate::diagnostics::Span::new(0, 0)),
                    |c| c.value.clone(),
                );
                let src = self.bind_value(&src_expr);
                RenderNode::Image {
                    attrs,
                    state_blocks,
                    src,
                }
            }
            // VectorIcon intrinsic → VectorMSDF pipeline. Content is an
            // asset handle (a `Str` path), like Image's source.
            "VectorIcon" => {
                let src_expr = el.content.first().map_or_else(
                    || Expr::StrLit(vec![], crate::diagnostics::Span::new(0, 0)),
                    |c| c.value.clone(),
                );
                let src = self.bind_value(&src_expr);
                RenderNode::Vector {
                    attrs: el.attrs.clone(),
                    src,
                }
            }
            // RFC-0020: the `Canvas` drawing surface. Its children are shape
            // commands, validated here (never silently ignored) and carried as
            // AST elements, the render walk re-evaluates their parameter
            // expressions every tick, which is what makes them reactive.
            "Canvas" => {
                self.errors
                    .extend(super::intrinsics::validate_canvas(el, &to_validate));
                let shapes = lower_canvas_items(el_children);
                RenderNode::Canvas {
                    attrs,
                    state_blocks,
                    shapes,
                    action: el.action.clone(),
                    env_snapshot: self.capture_env_snapshot(),
                }
            }
            // RFC-0017: the `Overlay` escape-hatch. Its children are lowered
            // normally, but the node itself carries them out of the parent flow
            //, the render walk defers them to the overlay layer.
            "Overlay" => {
                let children = self.lower_members(el_children, known_views);
                RenderNode::Overlay {
                    attrs,
                    children,
                    env_snapshot: self.capture_env_snapshot(),
                }
            }
            // Value widgets: resolve bound signal and keep as leaf nodes.
            // `Checkbox` (RFC-0018) joins them: a `bind: Bool` leaf that owns its
            // square-plus-checkmark visual and flips on tap/Space.
            "Toggle" | "Slider" | "TextField" | "Checkbox" => {
                let bound_sig = self.resolve_bind_sig(&attrs);
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children: Vec::new(),
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                    clip_path: None,
                }
            }
            // RFC-0018 `RadioButton`: like the value widgets, but its `value:` is a
            // literal identity (a `Str`), not a bound signal, only `bind:` names
            // the shared group `var`. Resolve *just* `bind` to the group signal;
            // `value` is read from the attrs at render time.
            "RadioButton" => {
                let bound_sig = self.resolve_group_bind_sig(&attrs);
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children: Vec::new(),
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                    clip_path: None,
                }
            }
            // RFC-0039: a package's native view. Reached here because the
            // catalog resolved the name (`validate_element` has already checked
            // its props and events against the entry the package declared), so
            // by this point it is an element like any other, with a Rust
            // implementation behind it instead of an intrinsic.
            name if byard_core::render::registry::info(name).is_some() => {
                let slot = self.mount_native_view(name, el.span);
                RenderNode::Native {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    action: el.action.clone(),
                    env_snapshot: self.capture_env_snapshot(),
                    slot,
                }
            }
            _ => {
                // Box / Column / Row / ScrollView and any other container.
                // RFC-0021 collapsing header: a `collapse_header` ScrollView mints
                // a `scroll_fraction` signal (0 = expanded, 1 = collapsed) scoped to
                // its *first* child (the header), the header's descendants read it
                // to interpolate their own size/opacity. The signal is carried on
                // `bound_sig` (unused by a ScrollView) so `render` can drive it.
                let collapse = el.name.as_str() == "ScrollView"
                    && Self::enum_prop(&attrs, "collapse_header") == Some("true");
                // RFC-0037 `clip(path)`: a `Clip`'s `path { … }` child is its
                // mask, not content. Lifted out here so it never reaches the
                // child list, which is what keeps `path` a hard error
                // everywhere else: the slot is claimed by exactly one parent,
                // not by a rule relaxed for everyone.
                let (clip_path, el_children) = self.lift_clip_path(el, el_children);
                let el_children: &[Member] = &el_children;
                let (children, bound_sig) = if collapse && !el_children.is_empty() {
                    let frac = self.ctx.create_signal(Value::Float(0.0));
                    let snap = self.env.len();
                    self.env
                        .push(Symbol::intern("scroll_fraction"), Value::Signal(frac));
                    let mut kids = self.lower_members(&el_children[..1], known_views);
                    self.env.truncate(snap);
                    kids.extend(self.lower_members(&el_children[1..], known_views));
                    (kids, Some(frac))
                } else {
                    (self.lower_members(el_children, known_views), None)
                };
                RenderNode::Box {
                    name: el.name.clone(),
                    attrs,
                    state_blocks,
                    anchor_name: el.anchor_name.clone(),
                    children,
                    action: el.action.clone(),
                    bound_sig,
                    env_snapshot: self.capture_env_snapshot(),
                    measure,
                    clip_path,
                }
            }
        }
    }

    /// Evaluates a `Clip`'s mask and opens it as a path clip (RFC-0037
    /// `clip(path)`), returning whether a clip was opened.
    ///
    /// The commands are measured from the `Clip`'s own top-left, the way a
    /// `Canvas`'s are from its own, so a mask is written in the same
    /// coordinates as the content it cuts. Tessellated through the same
    /// cache a filled path uses and keyed the same way, so a mask whose
    /// commands have not changed costs a lookup and not a tessellation.
    ///
    /// A mask that tessellates to nothing opens nothing, and that is not a
    /// silent failure dressed up: an outline with no area encloses no content,
    /// and the honest rendering of that is the clip's rectangle alone, which
    /// is what the enclosing entry already is.
    pub(super) fn begin_clip_path_mask(
        &mut self,
        mask: &ElementNode,
        rect: byard_core::frame::Rect,
        frame: &mut byard_core::frame::RenderFrame,
    ) -> bool {
        let origin = crate::interp::intrinsics::Rect::new(rect.x, rect.y, rect.width, rect.height);
        let commands = self.eval_path_commands(&mask.children, origin);
        if commands.len() < 2 {
            return false;
        }
        let even_odd = Self::shape_token(mask, "winding").as_deref() == Some("even_odd");
        let extent = rect.width.max(rect.height).max(1.0);
        self.open_mask(&commands, extent, even_odd, frame)
    }

    /// Opens a path clip over commands already in absolute pixels: the
    /// outline of a `Clip` under a rotated ancestor (RFC-0011).
    pub(super) fn begin_clip_commands(
        &mut self,
        commands: &[PathCommand],
        frame: &mut byard_core::frame::RenderFrame,
    ) -> bool {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for c in commands {
            if let PathCommand::Move(p) | PathCommand::Line(p) | PathCommand::Cubic(_, _, p) = c {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
        }
        let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
        self.open_mask(commands, extent, false, frame)
    }

    /// Tessellates `commands` through the shared mesh cache and opens the
    /// result as a path clip, returning whether anything was opened.
    fn open_mask(
        &mut self,
        commands: &[PathCommand],
        extent: f32,
        even_odd: bool,
        frame: &mut byard_core::frame::RenderFrame,
    ) -> bool {
        let tolerance = (extent / 800.0).clamp(0.05, 0.5);
        let key = path_fingerprint(commands, tolerance, even_odd);
        let mesh = if let Some(cached) = self.path_meshes.get_mut(&key) {
            cached.last_used = self.frame_seq;
            std::sync::Arc::clone(&cached.mesh)
        } else {
            let mesh = std::sync::Arc::new(tessellate_path(commands, tolerance, even_odd));
            self.tessellations += 1;
            self.path_meshes.insert(
                key,
                CachedMesh {
                    mesh: std::sync::Arc::clone(&mesh),
                    last_used: self.frame_seq,
                },
            );
            mesh
        };
        if mesh.indices.is_empty() {
            return false;
        }
        let [bx, by, bw, bh] = mesh.bounds;
        frame.begin_clip_path(byard_core::frame::ClipMask {
            mesh,
            bounds: byard_core::frame::Rect::new(bx, by, bw, bh),
        });
        true
    }

    /// Splits a `Clip`'s `path { … }` child off its content (RFC-0037
    /// `clip(path)`), returning the mask and the members that remain.
    ///
    /// Only a `Clip` has the slot, and only one: a second `path` is left in the
    /// children, where the ordinary validator reports it as a shape outside a
    /// `Canvas` with its existing message. Widening that rule to let a mask in
    /// would have let the next thing in by the same door. The mask's own body
    /// is checked here with the same validator a canvas path gets, so a
    /// misspelt command in a mask reads exactly as it would anywhere else.
    fn lift_clip_path(
        &mut self,
        el: &ElementNode,
        members: &[Member],
    ) -> (Option<std::rc::Rc<ElementNode>>, Vec<Member>) {
        if el.name.as_str() != "Clip" {
            return (None, members.to_vec());
        }
        let mut mask = None;
        let mut rest = Vec::with_capacity(members.len());
        for m in members {
            match m {
                Member::Element(child) if child.name.as_str() == "path" && mask.is_none() => {
                    self.errors
                        .extend(crate::interp::intrinsics::validate_path_body(child));
                    mask = Some(std::rc::Rc::new(child.clone()));
                }
                _ => rest.push(m.clone()),
            }
        }
        (mask, rest)
    }

    /// Lowers a slice of `Member`s into child `RenderNode`s, handling
    /// `Element`, `When`, and `For`.
    pub(super) fn lower_members(
        &mut self,
        members: &[Member],
        known_views: &[&str],
    ) -> Vec<RenderNode> {
        let mut nodes = Vec::new();
        for m in members {
            self.lower_member_into(m, known_views, &mut nodes);
        }
        nodes
    }

    fn lower_member_into(
        &mut self,
        member: &Member,
        known_views: &[&str],
        out: &mut Vec<RenderNode>,
    ) {
        match member {
            // A `content` reference inside a user-view body splices the slot the
            // current instance was called with (RFC-0007 D-A). The slot
            // nodes were pre-lowered in the caller scope.
            Member::Element(e)
                if e.name.as_str() == RESERVED_CONTENT && !self.slot_stack.is_empty() =>
            {
                if let Some(slot) = self.slot_stack.last() {
                    out.extend(slot.clone());
                }
            }
            Member::Element(e) if self.is_user_view_call(e) => {
                // A user-view call expands into its instantiated subtree, spliced
                // as siblings here (RFC-0007 §2).
                self.lower_user_view_call(e, known_views, out);
            }
            Member::Element(e) => {
                out.push(self.lower_element(e, known_views));
            }
            // RFC-0028 §4b: a lifecycle effect. Lowered here, in the scope it
            // was written in, and emitted as a node so its position in the
            // tree is what decides when it mounts.
            Member::Lifecycle {
                on_mount, action, ..
            } => {
                if let Ok(lowered) = self.lower_action(action, None) {
                    let kind = if *on_mount {
                        controller::EffectKind::Mount
                    } else {
                        controller::EffectKind::Unmount
                    };
                    let index = self.register_effect(kind, lowered);
                    out.push(RenderNode::Effect { index });
                }
            }
            // RFC-0029 §5: a timer effect. Same node, same mount rule and same
            // reply path as a lifecycle effect, which is the point: a timer
            // *is* a repeating mount-scoped effect.
            Member::Timer {
                every,
                dur_ms,
                action,
                ..
            } => {
                if let Ok(lowered) = self.lower_action(action, None) {
                    let index = self.register_effect(
                        controller::EffectKind::Timer {
                            every: *every,
                            dur_ms: *dur_ms,
                        },
                        lowered,
                    );
                    out.push(RenderNode::Effect { index });
                }
            }
            // `on change(a, b) => …`: one memo per watched value, read when
            // the effects settle.
            Member::Change {
                watched, action, ..
            } => {
                if let Ok(lowered) = self.lower_action(action, None) {
                    let watched = watched
                        .iter()
                        .map(|value| {
                            let compute = self.lower_expr(value, None);
                            self.ctx.open_memo(compute)
                        })
                        .collect();
                    let index =
                        self.register_effect(controller::EffectKind::Change { watched }, lowered);
                    out.push(RenderNode::Effect { index });
                }
            }
            // RFC-0026: a `route`/`tab` case only means something as a direct
            // child of its container, the nav lowering consumes those without
            // ever coming through here, so anything reaching this arm is
            // misplaced. Diagnosed rather than dropped (no silent failures).
            Member::Route { kind, span, .. } => {
                self.errors.push(CompileError::MisplacedNavCase {
                    span: *span,
                    keyword: kind.as_str().to_string(),
                    container: kind.container().to_string(),
                });
            }
            // RFC-0018 reactive `when`: bind the condition and register a branch
            // cache. Branches are lowered lazily on first selection (see
            // [`WhenPool`]) so an untaken recursive branch never lowers; the
            // driver re-reads the condition each frame and expands the taken one.
            Member::When {
                cond, then, els, ..
            } => {
                let cond_scope = self.bind_value(cond);
                let pool = self.when_pools.len();
                let env_snapshot = self.capture_env_snapshot();
                self.when_pools.push(WhenPool {
                    branch_spans: (
                        members_span(then),
                        els.as_deref().map_or(Span::new(0, 0), members_span),
                    ),
                    then_ast: then.clone(),
                    els_ast: els.clone().unwrap_or_default(),
                    known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
                    env_snapshot,
                    then: None,
                    els: None,
                    last_take: None,
                });
                out.push(RenderNode::When {
                    cond: cond_scope,
                    pool,
                });
            }
            // RFC-0018 reactive `for`: bind the list as a reactive projection and
            // register a body pool. Bodies are lowered lazily per slot during
            // reconciliation (never per frame); the driver renders one pooled body
            // per current element.
            Member::For {
                var,
                index,
                iter,
                body,
                ..
            } => {
                let list_scope = self.bind_value(iter);
                let pool = self.for_pools.len();
                let env_snapshot = self.capture_env_snapshot();
                self.for_pools.push(ForPool {
                    item_var: var.clone(),
                    index_var: index.clone(),
                    body: body.clone(),
                    known_views: known_views.iter().map(|s| (*s).to_string()).collect(),
                    env_snapshot,
                    item_slots: Vec::new(),
                    bodies: Vec::new(),
                    len: 0,
                });
                out.push(RenderNode::For {
                    pool,
                    list: list_scope,
                });
            }
            _ => {}
        }
    }
}
