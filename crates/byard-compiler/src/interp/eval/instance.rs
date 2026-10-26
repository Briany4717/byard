//! Instantiating a user `View` (RFC-0007, RFC-0019).
//!
//! A call to a view the author declared is expanded in place: the call's
//! positional and named arguments are bound to the callee's parameters, one
//! reactive memo each (and the diagnostics for a wrong arity, an unknown, a
//! missing or a duplicated parameter), a callback prop is registered as an
//! action the callee can fire, and the callee's body is lowered in a fresh
//! scope so the parent's environment is untouched. Instantiation depth is
//! bounded, so a guarded recursion that never terminates ends in a
//! diagnostic rather than a stack overflow.

use super::{InstanceBindings, Interpreter, RESERVED_CONTENT, RenderNode};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::Value;
use crate::interp::reactive::ScopeId;
use crate::parser::ast::{AttrKind, ElementNode, Expr, Param, Type, ViewDecl};
use crate::symbol::Symbol;
use crate::util::closest_match;

/// Maximum user-`View` instantiation depth before lowering truncates with a
/// diagnostic rather than risking a native stack overflow (RFC-0007 §4, D-C).
/// Far beyond any hand-written nesting, shallow enough to never
/// approach the stack limit. The static cycle check (`load_views`) catches
/// *unguarded* cycles at load; this bound is the backstop for a guarded
/// recursion whose runtime guard never terminates at lower time.
pub(super) const MAX_INSTANCE_DEPTH: u32 = 64;

/// The default-value expression of a parameter, if it declares one (RFC-0007
/// D-B). A parameter with a default is not required at the call site;
/// the default is evaluated in the callee scope when the argument is omitted.
fn param_default(param: &Param) -> Option<&Expr> {
    param.default.as_ref()
}

/// Whether a parameter is a callback prop, declared with a function type
/// `Fn(...)` (RFC-0019). Callback params bind a caller-supplied action block
/// rather than a projected value, so they take a separate binding path and are
/// skipped by the ordinary value-argument machinery in [`Interpreter::bind_args`].
fn is_callback_param(param: &Param) -> bool {
    matches!(param.ty, Some(Type::Function { .. }))
}

impl Interpreter {
    // ── argument → parameter binding (RFC-0007 §3) ──────────────────

    /// Projects one call argument into a reactive memo over the **parent** scope
    /// (the env active at the call site), so a parameter fed a parent `var` stays
    /// live (RFC-0004); a literal lowers to a constant memo with no dirty edges.
    fn project_arg(&mut self, expr: &Expr) -> ScopeId {
        let compute = self.lower_expr(expr, None);
        self.ctx.open_memo(compute)
    }

    /// Binds a single named argument (`name: value`, from `(...)` or `#[...]`) to
    /// the callee parameter of the same `Symbol`, filling `slots[i]` and emitting
    /// `UnknownParam`/`DuplicateParam` as needed (RFC-0007 §3/§6).
    fn bind_named_arg(
        &mut self,
        params: &[Param],
        callee: &str,
        name: &Symbol,
        value: &Expr,
        slots: &mut [Option<ScopeId>],
    ) {
        // The reserved `content` slot is filled by the child block, never a
        // named value.
        if name.as_str() == RESERVED_CONTENT {
            return;
        }
        match params.iter().position(|p| &p.name == name) {
            // A callback prop is bound separately (RFC-0019): it captures the
            // caller's action block, not a projected value, so leave its slot
            // empty here and let `bind_callbacks` handle it.
            Some(i) if is_callback_param(&params[i]) => {}
            Some(i) if slots[i].is_none() => {
                slots[i] = Some(self.project_arg(value));
            }
            Some(_) => self.errors.push(CompileError::DuplicateParam {
                span: value.span(),
                name: name.as_str().to_string(),
                callee: callee.to_string(),
            }),
            None => {
                let hint = closest_match(name.as_str(), params.iter().map(|p| p.name.as_str()))
                    .map(str::to_string);
                self.errors.push(CompileError::UnknownParam {
                    span: value.span(),
                    name: name.as_str().to_string(),
                    callee: callee.to_string(),
                    hint,
                });
            }
        }
    }

    /// Binds a user-view call's positional `content` and named `content`/`attrs`
    /// arguments to the callee's declared parameters, producing one reactive memo
    /// per bound parameter (RFC-0007 §3) and the §6 diagnostics
    /// (`ViewArityMismatch`/`UnknownParam`/`MissingParam`/`DuplicateParam`).
    ///
    /// Positional arguments (unnamed `(...)` entries) match by declaration order;
    /// named arguments (`name:` in `(...)` or `#[name: value]`) match by symbol.
    pub fn bind_args(&mut self, callee: &ViewDecl, call: &ElementNode) -> InstanceBindings {
        let params = &callee.params;
        let callee_name = callee.name.as_str().to_string();
        let mut slots: Vec<Option<ScopeId>> = vec![None; params.len()];
        // Positional arguments map only to *value* parameters; the reserved
        // `content` slot is filled by the child block, not a value.
        let value_param_idx: Vec<usize> = params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.name.as_str() != RESERVED_CONTENT && !is_callback_param(p))
            .map(|(i, _)| i)
            .collect();
        let mut positional_count = 0usize;
        let mut next_positional = 0usize;

        // 1) `(...)` content: unnamed → positional by order; named → by symbol.
        for arg in &call.content {
            if let Some(name) = &arg.name {
                self.bind_named_arg(params, &callee_name, name, &arg.value, &mut slots);
            } else {
                positional_count += 1;
                if let Some(&i) = value_param_idx.get(next_positional) {
                    next_positional += 1;
                    let scope = self.project_arg(&arg.value);
                    if slots[i].is_some() {
                        self.errors.push(CompileError::DuplicateParam {
                            span: arg.value.span(),
                            name: params[i].name.as_str().to_string(),
                            callee: callee_name.clone(),
                        });
                    } else {
                        slots[i] = Some(scope);
                    }
                }
                // Excess positional args are reported once via the arity check
                // below.
            }
        }

        // 2) `#[name: value]` attrs: named arguments (events are not parameters).
        for attr in &call.attrs {
            if let AttrKind::Prop { value } = &attr.kind {
                self.bind_named_arg(params, &callee_name, &attr.name, value, &mut slots);
            }
        }

        // 3) Arity: more positional args than the callee declares value
        //    parameters (RFC-0007 §6).
        if positional_count > value_param_idx.len() {
            self.errors.push(CompileError::ViewArityMismatch {
                span: call.span,
                name: callee_name.clone(),
                expected: value_param_idx.len(),
                found: positional_count,
            });
        }

        // 4) Missing required parameters: an unbound parameter with no default
        //   . The reserved `content` slot is never required, it
        //    defaults to an empty block.
        for (i, slot) in slots.iter().enumerate() {
            if slot.is_none()
                && param_default(&params[i]).is_none()
                && params[i].name.as_str() != RESERVED_CONTENT
                // Callback params are checked for presence in `bind_callbacks`.
                && !is_callback_param(&params[i])
            {
                self.errors.push(CompileError::MissingParam {
                    span: call.span,
                    name: params[i].name.as_str().to_string(),
                    callee: callee_name.clone(),
                });
            }
        }

        InstanceBindings {
            bindings: slots
                .into_iter()
                .enumerate()
                .filter_map(|(i, s)| s.map(|sc| (params[i].name.clone(), sc)))
                .collect(),
        }
    }

    // ── callback props (RFC-0019) ───────────────────────────────────────

    /// Registers a caller-supplied callback body in the shared `fn_table`,
    /// returning its [`AstId`]. The body is the caller's action block; it is
    /// lowered later, at the callee's invocation site, against the shared flat
    /// env, which still holds the caller's `var` bindings below the callee
    /// frame, so writes route to the caller's signals (RFC-0019 §2/§3).
    fn register_callback(&mut self, params: &[Symbol], body: &Expr) -> super::env::AstId {
        let id = super::env::AstId(u32::try_from(self.fn_table.len()).unwrap_or(u32::MAX));
        self.fn_table.push((params.to_vec(), body.clone(), true));
        id
    }

    /// Registers an arity-matched no-op callback (an empty action block with
    /// `arity` ignored parameters), used when a bare-identifier forward cannot
    /// be resolved to a live callback in the current lowering context. Matching
    /// the declared arity keeps the invocation-site arity check (§4) quiet.
    fn noop_callback(&mut self, arity: usize, span: Span) -> super::env::AstId {
        let params: Vec<Symbol> = (0..arity)
            .map(|i| Symbol::intern(&format!("__cb_arg{i}")))
            .collect();
        self.register_callback(&params, &Expr::Block(Vec::new(), span))
    }

    /// The caller-supplied argument expression for a named parameter, a `name:`
    /// entry in the `(...)` content or a `#[name: value]` attribute. Callback
    /// props are always passed by name.
    fn find_named_arg<'a>(&self, call: &'a ElementNode, name: &Symbol) -> Option<&'a Expr> {
        call.content
            .iter()
            .find(|a| a.name.as_ref() == Some(name))
            .map(|a| &a.value)
            .or_else(|| {
                call.attrs.iter().find_map(|attr| match &attr.kind {
                    AttrKind::Prop { value } if &attr.name == name => Some(value),
                    _ => None,
                })
            })
    }

    /// Binds a callback-prop parameter (RFC-0019): pushes a `Value::Fn` naming
    /// the caller's action block (or the `= { … }` default, or a forwarded
    /// callback already in scope). Emits the §4 diagnostics, arity mismatch
    /// between the `Fn(...)` type and the block's `|params|`, a non-callback
    /// argument, or a missing required callback.
    fn bind_callback_param(&mut self, param: &Param, call: &ElementNode) {
        let arg_ty_count = match &param.ty {
            Some(Type::Function { params, .. }) => params.len(),
            _ => 0,
        };
        if let Some(arg) = self.find_named_arg(call, &param.name) {
            match arg {
                Expr::Lambda {
                    params, body, span, ..
                } => {
                    if params.len() != arg_ty_count {
                        self.errors.push(CompileError::CallbackArityMismatch {
                            span: *span,
                            name: param.name.as_str().to_string(),
                            expected: arg_ty_count,
                            found: params.len(),
                        });
                    }
                    let id = self.register_callback(params, body);
                    self.env.push(param.name.clone(), Value::Fn(id));
                }
                // Forwarding: `on_tap: outer_on_tap` re-binds a callback already
                // in scope (a wrapper forwarding its own callback prop inward).
                // A bare identifier that does *not* currently resolve to a
                // callback is bound to an arity-matched no-op rather than a hard
                // type error, a wrapper checked in isolation has its own
                // callback params unbound, and that must not false-positive.
                Expr::Ident(other, span) => {
                    if let Some(&Value::Fn(id)) = self.env.lookup(other) {
                        self.env.push(param.name.clone(), Value::Fn(id));
                    } else {
                        let id = self.noop_callback(arg_ty_count, *span);
                        self.env.push(param.name.clone(), Value::Fn(id));
                    }
                }
                other => self.errors.push(CompileError::CallbackTypeMismatch {
                    span: other.span(),
                    callee: call.name.as_str().to_string(),
                    name: param.name.as_str().to_string(),
                }),
            }
        } else if let Some(Expr::Lambda { params, body, .. }) = param_default(param) {
            // The default is an action block (`= {}` / `= {|_|}`); register it.
            let id = self.register_callback(params, body);
            self.env.push(param.name.clone(), Value::Fn(id));
        } else {
            // A required callback with no default and no argument.
            self.errors.push(CompileError::MissingParam {
                span: call.span,
                name: param.name.as_str().to_string(),
                callee: call.name.as_str().to_string(),
            });
        }
    }

    /// Captures the instance environment for a box being lowered (RFC-0019 §2),
    /// or an empty snapshot at the top level. Only boxes lowered *inside* a
    /// user-view instance need one, a top-level box's event actions re-lower
    /// against the persistent root env, exactly as before, so its snapshot stays
    /// empty and render behaviour is unchanged.
    pub(super) fn capture_env_snapshot(&self) -> Vec<(Symbol, Value)> {
        if self.instance_depth == 0
            && self.for_depth == 0
            && self.nav_depth == 0
            && self.restored_depth == 0
        {
            Vec::new()
        } else {
            self.env.snapshot()
        }
    }

    /// Whether `el` is a user-`View` call: a name that resolves in the view
    /// table and is not an RFC-0005 intrinsic, which always wins.
    pub(super) fn is_user_view_call(&self, el: &ElementNode) -> bool {
        super::intrinsics::lookup(el.name.as_str()).is_none() && self.view_table.contains(&el.name)
    }

    /// Expands a user-`View` call site into its instantiated subtree, spliced as
    /// siblings at `out` (RFC-0007 §2). Opens a fresh instance scope holding the
    /// argument bindings plus the callee's own local `var`/`let`/`fn`
    /// (isolated per instance), lowers the callee body, then truncates the scope
    /// so the parent environment is untouched.
    pub(super) fn lower_user_view_call(
        &mut self,
        el: &ElementNode,
        known_views: &[&str],
        out: &mut Vec<RenderNode>,
    ) {
        // A user view is not validated by the intrinsic contract; its argument
        // diagnostics come from `bind_args` (RFC-0007 §3/§6).
        let Some(id) = self.view_table.resolve(&el.name) else {
            // Unreachable in practice (caller checked), but degrade gracefully.
            out.push(self.lower_element(el, known_views));
            return;
        };
        // Own the callee so the `&self.view_table` borrow does not conflict with
        // the `&mut self` lowering below (the table is `Send` and owns its declarations).
        let callee = self.view_table.decl(id).clone();

        // Runtime depth bound (RFC-0007 §4): a guarded recursion whose
        // guard never terminates at lower time is truncated with a diagnostic
        // rather than overflowing the native stack.
        if self.instance_depth >= MAX_INSTANCE_DEPTH {
            self.errors.push(CompileError::RecursiveView {
                span: el.span,
                path: format!(
                    "{} (instantiation depth bound {MAX_INSTANCE_DEPTH} exceeded)",
                    el.name.as_str()
                ),
            });
            return; // truncate the subtree; never recurse past the bound
        }
        self.instance_depth += 1;

        // Slot (RFC-0007 D-A): a `{ ... }` block is allowed only when
        // the callee declares a `content` parameter; the block is pre-lowered in
        // the *caller* scope (capturing caller `var`s) and pushed as this
        // instance's slot. A block passed to a slot-less callee is
        // `UnexpectedChildren`.
        let has_content_param = callee
            .params
            .iter()
            .any(|p| p.name.as_str() == RESERVED_CONTENT);
        let slot_nodes = if el.children.is_empty() {
            Vec::new()
        } else if has_content_param {
            self.lower_members(&el.children, known_views)
        } else {
            self.errors.push(CompileError::UnexpectedChildren {
                span: el.span,
                name: el.name.as_str().to_string(),
            });
            Vec::new()
        };

        // 1) Bind arguments → parameters in the *parent* scope (RFC-0007 §3).
        let bindings = self.bind_args(&callee, el);

        // 2) Open the per-instance lexical frame (D-D): push each
        //    parameter in declaration order, a bound argument's memo, else a
        //    default evaluated in the callee scope, then the callee's
        //    own declarations.
        let snapshot = self.env.len();
        for param in &callee.params {
            // A callback prop (RFC-0019) binds the caller's action block as a
            // `Value::Fn`, resolved at invocation in `lower_call`, rather than a
            // projected value memo.
            if is_callback_param(param) {
                self.bind_callback_param(param, el);
                continue;
            }
            if let Some((_, scope)) = bindings.bindings.iter().find(|(n, _)| n == &param.name) {
                self.env.push(param.name.clone(), Value::Memo(*scope));
            } else if let Some(default) = param_default(param) {
                // Lowered in the current (callee) frame, so a default may
                // reference earlier parameters.
                let scope = self.project_arg(default);
                self.env.push(param.name.clone(), Value::Memo(scope));
            }
            // An unbound, defaultless parameter already produced a `MissingParam`
            // diagnostic in `bind_args`; leave it unbound.
        }
        // Local `var`/`let`/`fn`/`inject` open in the instance frame, so two
        // instances of the same view keep independent state.
        self.eval_view_decls(&callee);

        // 3) Lower the callee body and splice the roots as siblings (RFC-0007
        //    §2 step 5; reuses the multi-node `when`/`for` splice shape). The
        //    slot is live for `content` references within the body.
        self.slot_stack.push(slot_nodes);
        let nodes = self.lower_members(&callee.body, known_views);
        self.slot_stack.pop();
        out.extend(nodes);

        // 4) Close the instance scope (RFC-0007 §2 step 6).
        self.env.truncate(snapshot);
        self.instance_depth -= 1;
    }
}
