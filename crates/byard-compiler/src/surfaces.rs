//! Where a surface may be written (RFC-0068 §1), and whether a program has
//! more than one.
//!
//! A program with no `Window` is *single-surface*: its entry view is the
//! content of the one implicit window, as it has always been, and nothing here
//! is built for it. A program that writes `Window` is *multi-surface*, and
//! then the *surface level* of its entry view (the body, the bodies of `when`
//! and `for` blocks at that level, and the body of a view called there) may
//! hold nothing but windows and the control flow and calls that lead to them.
//!
//! The rule is structural and every view it involves is visible at check time,
//! so the checker enforces all of it: [`MisplacedSurface`] for a `Window`
//! anywhere else, [`ContentOutsideSurface`] for an element at the surface
//! level that is not one, [`OpeningSizeIsStatic`] and [`SurfaceForNeedsKey`]
//! as warnings.
//!
//! [`MisplacedSurface`]: CompileError::MisplacedSurface
//! [`ContentOutsideSurface`]: CompileError::ContentOutsideSurface
//! [`OpeningSizeIsStatic`]: CompileError::OpeningSizeIsStatic
//! [`SurfaceForNeedsKey`]: CompileError::SurfaceForNeedsKey

use std::collections::{HashMap, HashSet};

use crate::diagnostics::{CompileError, Span};
use crate::interp::intrinsics;
use crate::parser::ast::{AttrKind, ElementNode, Expr, Member, ViewDecl, root_view};
use crate::symbol::Symbol;

/// The element that is a window of its own.
pub const WINDOW: &str = "Window";

/// The element that is a layer surface (RFC-0068 §3).
pub const PANEL: &str = "Panel";

/// Whether `name` is an element that is a surface of its own.
///
/// `Window` and `Panel` (RFC-0068 §3).
#[must_use]
pub fn is_surface_element(name: &str) -> bool {
    name == WINDOW || name == PANEL
}

/// What surfaces a program writes, found by reading it, not by running it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceShape {
    /// A `Window` is written at the surface level.
    pub has_window: bool,
    /// A `Panel` is written at the surface level: the shell backend is chosen
    /// by it (RFC-0068 §8).
    pub has_panel: bool,
}

impl SurfaceShape {
    /// Whether the program is multi-surface: it writes at least one surface
    /// element, so its entry view is a list of surfaces and not the content of
    /// the one implicit window.
    #[must_use]
    pub const fn is_multi(self) -> bool {
        self.has_window || self.has_panel
    }
}

/// The surfaces `views` write, without checking anything.
#[must_use]
pub fn surface_shape(views: &[ViewDecl]) -> SurfaceShape {
    let mut sink = Vec::new();
    check_surfaces(views, &mut sink)
}

/// The surfaces the program writes when `entry` is the view it draws: the
/// entry view `byard shot --view` picks is not always `Main`.
#[must_use]
pub fn surface_shape_of(views: &[ViewDecl], entry: &ViewDecl) -> SurfaceShape {
    let mut sink = Vec::new();
    check_surfaces_from(views, entry, &mut sink)
}

/// Checks the surface rules over `views` and says what the program is.
pub fn check_surfaces(views: &[ViewDecl], errors: &mut Vec<CompileError>) -> SurfaceShape {
    let Some(entry) = root_view(views) else {
        return SurfaceShape::default();
    };
    check_surfaces_from(views, entry, errors)
}

fn check_surfaces_from(
    views: &[ViewDecl],
    entry: &ViewDecl,
    errors: &mut Vec<CompileError>,
) -> SurfaceShape {
    let by_name: HashMap<&Symbol, &ViewDecl> = views.iter().map(|v| (&v.name, v)).collect();
    let mut walk = Walk {
        by_name,
        leads: HashMap::new(),
        level: HashSet::new(),
        errors,
        shape: SurfaceShape::default(),
    };
    let multi = walk.members_lead(&entry.body, &mut Vec::new());
    if multi {
        walk.surface_level(entry, &mut Vec::new());
    }
    // A `Window` at the surface level of a view called there is fine; one
    // anywhere else is not, in any view.
    for view in views {
        walk.misplaced(&view.body);
    }
    walk.shape
}

struct Walk<'a, 'e> {
    by_name: HashMap<&'a Symbol, &'a ViewDecl>,
    /// Whether a view's surface level leads to a surface, by name.
    leads: HashMap<Symbol, bool>,
    /// The spans of the `Window` elements found at the surface level.
    level: HashSet<Span>,
    errors: &'e mut Vec<CompileError>,
    shape: SurfaceShape,
}

impl<'a> Walk<'a, '_> {
    /// The view an element calls, when it is a call of one of the program's
    /// views and not an intrinsic.
    fn call(&self, el: &ElementNode) -> Option<&'a ViewDecl> {
        if intrinsics::lookup(el.name.as_str()).is_some() {
            return None;
        }
        self.by_name.get(&el.name).copied()
    }

    /// Whether a view's surface level leads to a window.
    fn view_leads(&mut self, view: &'a ViewDecl, stack: &mut Vec<Symbol>) -> bool {
        if let Some(known) = self.leads.get(&view.name) {
            return *known;
        }
        // A view reached again while it is being read is a recursion; it adds
        // nothing the first visit has not.
        if stack.contains(&view.name) {
            return false;
        }
        stack.push(view.name.clone());
        let leads = self.members_lead(&view.body, stack);
        stack.pop();
        self.leads.insert(view.name.clone(), leads);
        leads
    }

    /// Whether the surface level of `members` leads to a window.
    fn members_lead(&mut self, members: &'a [Member], stack: &mut Vec<Symbol>) -> bool {
        let mut any = false;
        for member in members {
            any |= match member {
                Member::Element(el) if is_surface_element(el.name.as_str()) => true,
                Member::Element(el) => match self.call(el) {
                    Some(view) => self.view_leads(view, stack),
                    None => false,
                },
                Member::When { then, els, .. } => {
                    let a = self.members_lead(then, stack);
                    let b = els.as_ref().is_some_and(|e| self.members_lead(e, stack));
                    a || b
                }
                Member::For { body, .. } => self.members_lead(body, stack),
                _ => false,
            };
        }
        any
    }

    /// The rules for the surface level of `view`, a program that has windows:
    /// no element but a window, a view call that leads to one, or control flow
    /// around them.
    fn surface_level(&mut self, view: &'a ViewDecl, stack: &mut Vec<Symbol>) {
        if stack.contains(&view.name) {
            return;
        }
        stack.push(view.name.clone());
        self.level_members(&view.body, view, stack);
        stack.pop();
    }

    fn level_members(
        &mut self,
        members: &'a [Member],
        view: &'a ViewDecl,
        stack: &mut Vec<Symbol>,
    ) {
        for member in members {
            match member {
                Member::Element(el) if is_surface_element(el.name.as_str()) => {
                    self.level.insert(el.span);
                    if el.name.as_str() == PANEL {
                        self.shape.has_panel = true;
                        self.panel_attrs(el, view);
                    } else {
                        self.shape.has_window = true;
                        self.window_attrs(el, view);
                    }
                }
                Member::Element(el) => match self.call(el) {
                    Some(callee) if self.view_leads(callee, &mut Vec::new()) => {
                        self.surface_level(callee, stack);
                    }
                    _ => self.errors.push(CompileError::ContentOutsideSurface {
                        span: el.span,
                        element: el.name.as_str().to_string(),
                    }),
                },
                Member::When { then, els, .. } => {
                    self.level_members(then, view, stack);
                    if let Some(els) = els {
                        self.level_members(els, view, stack);
                    }
                }
                Member::For {
                    key, body, span, ..
                } => {
                    if key.is_none() && self.members_lead(body, &mut Vec::new()) {
                        self.errors
                            .push(CompileError::SurfaceForNeedsKey { span: *span });
                    }
                    self.level_members(body, view, stack);
                }
                _ => {}
            }
        }
    }

    /// `OpeningSizeIsStatic`: a `width` or `height` that reads something that
    /// can change.
    fn window_attrs(&mut self, el: &ElementNode, view: &ViewDecl) {
        let reactive: HashSet<&Symbol> = view
            .params
            .iter()
            .map(|p| &p.name)
            .chain(view.body.iter().filter_map(|m| match m {
                Member::Var { name, .. } | Member::Let { name, .. } => Some(name),
                _ => None,
            }))
            .collect();
        for attr in &el.attrs {
            if !matches!(attr.name.as_str(), "width" | "height") {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            let mut reads = false;
            let mut probe = Member::Expr(value.clone());
            crate::resolve::walk::member_exprs(&mut probe, &mut |e| {
                if let Expr::Ident(name, _) = e {
                    reads |= reactive.contains(name) || name.as_str() == "window";
                }
            });
            if reads {
                self.errors.push(CompileError::OpeningSizeIsStatic {
                    span: value.span(),
                    attr: attr.name.as_str().to_string(),
                });
            }
        }
    }

    /// The rules of a panel's attributes (RFC-0068 §3): `ExclusiveNeedsEdge`,
    /// `RecreatesSurface` and `PanelTakesNoKeys`. `edge` with `anchor` is
    /// `InvalidAttributeCombination`, found with the element's other
    /// combinations.
    fn panel_attrs(&mut self, el: &'a ElementNode, view: &ViewDecl) {
        // State that changes: a `var` or a `let` of the view. A parameter or a
        // loop variable is how a panel is given its screen, and is not it.
        let reactive: HashSet<&Symbol> = view
            .body
            .iter()
            .filter_map(|m| match m {
                Member::Var { name, .. } | Member::Let { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        let prop = |name: &str| {
            el.attrs.iter().find_map(|a| match &a.kind {
                AttrKind::Prop { value } if a.name.as_str() == name => Some(value),
                _ => None,
            })
        };
        let word = |name: &str| match prop(name) {
            Some(Expr::Ident(w, _)) => Some(w.as_str().to_string()),
            _ => None,
        };
        for attr in ["namespace", "screen"] {
            let Some(value) = prop(attr) else { continue };
            let mut reads = false;
            let mut probe = Member::Expr(value.clone());
            crate::resolve::walk::member_exprs(&mut probe, &mut |e| {
                if let Expr::Ident(name, _) = e {
                    reads |= reactive.contains(name);
                }
            });
            if reads {
                self.errors.push(CompileError::RecreatesSurface {
                    span: value.span(),
                    attr: attr.to_string(),
                });
            }
        }
        if word("exclusive").as_deref() == Some("true") && prop("edge").is_none() {
            let anchors = match prop("anchor") {
                Some(Expr::Tuple(args, _)) => args.len(),
                Some(_) => 1,
                None => 0,
            };
            if anchors < 3 {
                if let Some(value) = prop("exclusive") {
                    self.errors
                        .push(CompileError::ExclusiveNeedsEdge { span: value.span() });
                }
            }
        }
        if matches!(word("keyboard").as_deref(), None | Some("none")) {
            let mut seen = Vec::new();
            self.takes_keys(&el.children, &mut seen, &mut Vec::new());
        }
    }

    /// `PanelTakesNoKeys`: a `TextField` or a `key_down` handler in `members`,
    /// looking through `when`, `for` and the views called.
    fn takes_keys(&mut self, members: &'a [Member], seen: &mut Vec<Span>, stack: &mut Vec<Symbol>) {
        for member in members {
            match member {
                Member::Element(el) => {
                    let what = if el.name.as_str() == "TextField" {
                        Some("TextField")
                    } else if el.attrs.iter().any(|a| a.name.as_str() == "key_down") {
                        Some("key_down")
                    } else {
                        None
                    };
                    if let Some(what) = what {
                        if !seen.contains(&el.span) {
                            seen.push(el.span);
                            self.errors.push(CompileError::PanelTakesNoKeys {
                                span: el.span,
                                what: what.to_string(),
                            });
                        }
                    }
                    self.takes_keys(&el.children, seen, stack);
                    for slot in el.slot_members() {
                        self.takes_keys(slot, seen, stack);
                    }
                    if let Some(view) = self.call(el) {
                        if !stack.contains(&view.name) {
                            stack.push(view.name.clone());
                            self.takes_keys(&view.body, seen, stack);
                            stack.pop();
                        }
                    }
                }
                Member::When { then, els, .. } => {
                    self.takes_keys(then, seen, stack);
                    if let Some(els) = els {
                        self.takes_keys(els, seen, stack);
                    }
                }
                Member::For { body, .. } => self.takes_keys(body, seen, stack),
                _ => {}
            }
        }
    }

    /// `MisplacedSurface`: every `Window` that is not at a surface level.
    fn misplaced(&mut self, members: &'a [Member]) {
        for member in members {
            match member {
                Member::Element(el) => {
                    if is_surface_element(el.name.as_str()) && !self.level.contains(&el.span) {
                        self.errors.push(CompileError::MisplacedSurface {
                            span: el.span,
                            element: el.name.as_str().to_string(),
                        });
                    }
                    self.misplaced(&el.children);
                    for slot in el.slot_members() {
                        self.misplaced(slot);
                    }
                }
                Member::When { then, els, .. } => {
                    self.misplaced(then);
                    if let Some(els) = els {
                        self.misplaced(els);
                    }
                }
                Member::For { body, .. } | Member::Route { body, .. } => {
                    self.misplaced(body);
                }
                _ => {}
            }
        }
    }
}
