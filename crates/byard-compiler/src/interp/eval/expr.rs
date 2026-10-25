//! Lowering expressions and actions (RFC-0004, RFC-0019).
//!
//! Each `byld` expression becomes a `FnMut(&mut ReactiveCtx) -> Value` that
//! resolves its identifiers at lowering time and performs its reads through
//! the context at run time, so read-tracking stays dynamic. This is that
//! lowering (identifiers, calls, the collection methods, string
//! interpolation and the theme members), the operators it evaluates with, the
//! pure evaluation used to seed a `var` or an action operand, and the
//! lowering of an action: a mutation, a bare expression, or a callback body.

use super::animation::mix_hex_oklab;
use super::{CURRENT_PAYLOAD, Interpreter, LAMBDA_ARGS, Lowered};
use crate::diagnostics::{CompileError, Span};
use crate::interp::env::{Env, SignalId, Value};
use crate::interp::events::Action;
use crate::interp::reactive::untrack;
use crate::parser::ast::{AssignOp, BinOp, Expr, PostfixOp, StrPart, UnOp};
use crate::symbol::Symbol;

/// The value of a numeric literal, exactly as [`Interpreter::lower_expr`]
/// would compute it, or `None` for any other expression. Literals need no
/// context, so [`Interpreter::eval_pure`] answers them without lowering.
const fn numeric_literal(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::IntLit(n, _) => Some(Value::Int(*n)),
        // An angle is already in radians (RFC-0011 T1), a plain `Float`.
        Expr::FloatLit(f, _) | Expr::AngleLit(f, _) => Some(Value::Float(*f)),
        _ => None,
    }
}

/// Multiplies a colour's alpha by `opacity`, folds an element's effective
/// opacity into the widget/text primitives it emits so a translucent control
/// dims as a whole, not just its background (RFC-0011 T4 approximation).
/// Evaluates one binary arithmetic operation (`+ - * /`, RFC-0020 enabler)
/// with numeric promotion: Int∘Int → Int (division truncates), any Float
/// operand → Float. Division by zero yields the zero of the promoted type and
/// a non-numeric operand yields [`Value::Unit`], the logic thread never
/// panics on user expressions. Pure and unit-testable.
pub(super) fn eval_binary(op: BinOp, lhs: Value, rhs: Value) -> Value {
    match op {
        // Comparison (RFC-0027 §1) → Bool.
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            eval_compare(op, &lhs, &rhs)
        }
        // `&&`/`||` never reach here (lowered as control flow); the arithmetic
        // path handles `+ - * /` with string/list concat on `+`.
        // `??` is lowered as control flow too; reaching here it is its own
        // answer.
        BinOp::Otherwise => match lhs {
            Value::Unit => rhs,
            value => value,
        },
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::And | BinOp::Or => {
            match (lhs, rhs) {
                (Value::Int(a), Value::Int(b)) => Value::Int(match op {
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    BinOp::Div => {
                        if b == 0 {
                            0
                        } else {
                            a.wrapping_div(b)
                        }
                    }
                    _ => 0,
                }),
                (Value::Int(a), Value::Float(b)) => eval_binary_f(op, a as f64, b),
                (Value::Float(a), Value::Int(b)) => eval_binary_f(op, a, b as f64),
                (Value::Float(a), Value::Float(b)) => eval_binary_f(op, a, b),
                // String concat / list concat (RFC-0027 §3/§4): `+` only.
                (a, b) if matches!(op, BinOp::Add) => eval_concat(a, b),
                _ => Value::Unit,
            }
        }
    }
}

/// String and list concatenation (RFC-0027 §3/§4). A `Str` on either side
/// coerces the other operand through the shared scalar formatter
/// ([`format_scalar`]); two `List`s concatenate; anything else is `Unit` (the
/// checker reports the mismatch).
pub(super) fn eval_concat(a: Value, b: Value) -> Value {
    // A `List` operand only concatenates with another `List`, it never string-
    // coerces (RFC-0027 §3). A `Str` on either side coerces the other *scalar*.
    match (a, b) {
        (Value::List(mut xs), Value::List(ys)) => {
            xs.extend(ys);
            Value::List(xs)
        }
        (Value::Str(mut s), other) if is_scalar(&other) => {
            s.push_str(&format_scalar(&other));
            Value::Str(s)
        }
        (other, Value::Str(s)) if is_scalar(&other) => {
            let mut out = format_scalar(&other);
            out.push_str(&s);
            Value::Str(out)
        }
        _ => Value::Unit,
    }
}

/// Whether a value is a formattable scalar (`Int`/`Float`/`Bool`/`Str`), the
/// operands `Str + _` will coerce (RFC-0027 §3). `List`/`Record`/`Unit` are not.
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Str(_)
    )
}

/// Comparison (RFC-0027 §1) → `Bool`. Numeric operands compare with Int→Float
/// promotion; `Str` by value/lexicographic order; `Bool` by `==`/`!=` only;
/// `List`/`Record` by structural equality (ordering on them is a checker
/// `TypeMismatch`, handled there). Incompatible operands degrade to
/// `Bool(false)` at runtime (never a panic; the checker reports the mismatch).
pub(super) fn eval_compare(op: BinOp, a: &Value, b: &Value) -> Value {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => {
            // Bool supports eq only; ordering is meaningless → not-equal-based.
            return Value::Bool(match op {
                BinOp::Eq => x == y,
                BinOp::Ne => x != y,
                _ => false,
            });
        }
        // Structural equality for List/Record (and any other same-shape pair).
        _ => {
            let eq = structural_eq(a, b);
            return Value::Bool(match op {
                BinOp::Eq => eq,
                BinOp::Ne => !eq,
                _ => false,
            });
        }
    };
    let Some(ord) = ord else {
        return Value::Bool(false);
    };
    Value::Bool(match op {
        BinOp::Eq => ord == Ordering::Equal,
        BinOp::Ne => ord != Ordering::Equal,
        BinOp::Lt => ord == Ordering::Less,
        BinOp::Le => ord != Ordering::Greater,
        BinOp::Gt => ord == Ordering::Greater,
        BinOp::Ge => ord != Ordering::Less,
        _ => false,
    })
}

/// Structural (element/field-wise) equality for lists and records (RFC-0027
/// §1), with numeric Int↔Float promotion at the leaves.
pub(super) fn structural_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => (x - y).abs() < f64::EPSILON,
        (Value::Int(x), Value::Float(y)) | (Value::Float(y), Value::Int(x)) => {
            (*x as f64 - *y).abs() < f64::EPSILON
        }
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| structural_eq(x, y))
        }
        (Value::Record(xs), Value::Record(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((kx, vx), (ky, vy))| kx == ky && structural_eq(vx, vy))
        }
        _ => false,
    }
}

/// `color` with its alpha set to `amount` (0 to 1, clamped), as an
/// alpha-first `0xAARRGGBB` carrying the has-alpha tag, so even a fully
/// transparent result reads as transparent and not as an opaque `0xRRGGBB`.
fn with_alpha(color: i64, amount: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let byte = (amount.clamp(0.0, 1.0) * 255.0).round() as i64;
    (color & 0x00FF_FFFF) | (byte << 24) | crate::lexer::COLOR_HAS_ALPHA_TAG
}

/// Percent-encodes `text` for use as one value in a URL (RFC 3986): letters,
/// digits and `-`, `.`, `_`, `~` stay, and every other byte of its UTF-8
/// becomes `%XX`. A space is `%20`, which is right in a query and in a path.
fn url_encoded(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The name an assignment writes, for a message: `found` in `found = …`.
fn target_name(target: &Expr) -> String {
    match target {
        Expr::Ident(name, _) => name.as_str().to_string(),
        _ => "a var".to_string(),
    }
}

/// Resolves a data member access (RFC-0027 §4/§6): `xs.len` (list/string
/// length → `Int`) or `r.field` (record field → its value). Anything else
/// degrades to `Unit` (the checker reports it; no panic).
fn data_member(base: &Value, field: &Symbol) -> Value {
    let f = field.as_str();
    match base {
        Value::List(xs) if f == "len" => Value::Int(i64::try_from(xs.len()).unwrap_or(i64::MAX)),
        Value::Str(s) if f == "len" => {
            Value::Int(i64::try_from(s.chars().count()).unwrap_or(i64::MAX))
        }
        Value::Record(fields) => fields
            .iter()
            .find(|(k, _)| k.as_str() == f)
            .map_or(Value::Unit, |(_, v)| v.clone()),
        _ => Value::Unit,
    }
}

/// Runs `f` with `elem` installed as the current per-element/lambda binding
/// (RFC-0027 §5), reusing the payload slot the lambda body was lowered against.
/// The previous slot value is saved and restored so a `map`/`filter` nested in
/// an event action never clobbers that action's payload.
/// The order `sort`, `sortBy`, `min` and `max` use (RFC-0027): numbers by
/// value (an `Int` and a `Float` compare as numbers), text by code point,
/// `false` before `true`. Anything else is equal to everything, so a stable
/// sort leaves it where it was rather than inventing an order.
fn value_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    #[allow(clippy::cast_precision_loss)]
    let num = |v: &Value| match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => Ordering::Equal,
        },
    }
}

fn with_lambda_elem<F: FnOnce() -> Value>(elem: Value, f: F) -> Value {
    let prev = CURRENT_PAYLOAD.with(|cell| cell.borrow_mut().replace(elem));
    let out = f();
    CURRENT_PAYLOAD.with(|cell| *cell.borrow_mut() = prev);
    out
}

/// Indexes `base[index]` (RFC-0027 §4): a `List` at an in-range integer index
/// yields the element; out-of-range or non-list/non-int degrades to `Unit`
/// (never a panic). Negative indices are out of range.
pub(super) fn index_value(base: &Value, index: &Value) -> Value {
    match (base, index) {
        (Value::List(xs), Value::Int(i)) => usize::try_from(*i)
            .ok()
            .and_then(|i| xs.get(i))
            .cloned()
            .unwrap_or(Value::Unit),
        _ => Value::Unit,
    }
}

/// The Float leg of [`eval_binary`]. `x / 0.0` yields `0.0`, not an
/// IEEE infinity/NaN, a NaN sweep or width would poison layout and paint.
fn eval_binary_f(op: BinOp, a: f64, b: f64) -> Value {
    Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => {
            if b == 0.0 {
                0.0
            } else {
                a / b
            }
        }
        _ => 0.0,
    })
}

/// The single scalar-formatting function (RFC-0027 §3): the display form used
/// by both `Text("{x}")` interpolation and `Str + scalar` concatenation, so the
/// two paths agree byte-for-byte. Non-scalar values format to an empty string.
fn format_scalar(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.clone(),
        _ => String::new(),
    }
}

/// A number with exactly `decimals` decimals, for `{x:.N}` (RFC-0027).
///
/// A value that rounds to zero is written without a sign: a forecast of
/// `-0.2` shown with no decimals is `0`, not `-0`. Anything that is not a
/// number is written as `{x}` would write it.
fn format_fixed(v: &Value, decimals: u8) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        other => return format_scalar(other),
    };
    let s = format!("{f:.*}", usize::from(decimals));
    match s.strip_prefix('-') {
        Some(abs) if abs.bytes().all(|b| matches!(b, b'0' | b'.')) => abs.to_string(),
        _ => s,
    }
}

impl Interpreter {
    // ── lowering ────────────────────────────────────────────────────────

    /// Lowers `expr` to a reactive computation against the current environment.
    pub(super) fn lower_expr(&mut self, expr: &Expr, payload_name: Option<&Symbol>) -> Lowered {
        match expr {
            Expr::IntLit(n, _) => {
                let n = *n;
                Box::new(move |_| Value::Int(n))
            }
            Expr::FloatLit(f, _) => {
                let f = *f;
                Box::new(move |_| Value::Float(f))
            }
            // Already canonicalized to radians by the lexer (RFC-0011 T1),
            // from here on an angle is just a plain `Float`.
            Expr::AngleLit(rad, _) => {
                let rad = *rad;
                Box::new(move |_| Value::Float(rad))
            }
            Expr::StrLit(parts, _) => self.lower_strlit(parts, payload_name),
            Expr::Ident(name, span) => {
                // A bare reference to a callback prop (RFC-0019 §4), reached only
                // when it is *not* the callee of a call (`on_tap()` is handled in
                // `lower_call`), is invalid: callbacks are fire-and-forget, not
                // first-class values.
                if let Some(&Value::Fn(id)) = self.env.lookup(name) {
                    if self.fn_table.get(id.0 as usize).is_some_and(|e| e.2) {
                        self.errors.push(CompileError::CallbackNotInvocable {
                            span: *span,
                            name: name.as_str().to_string(),
                        });
                    }
                }
                self.lower_ident(name, payload_name)
            }
            Expr::Array(elems, _) => {
                let mut cs: Vec<Lowered> = elems
                    .iter()
                    .map(|e| self.lower_expr(e, payload_name))
                    .collect();
                Box::new(move |ctx| Value::List(cs.iter_mut().map(|c| c(ctx)).collect()))
            }
            Expr::Tuple(elems, _) => {
                let mut cs: Vec<(Option<Symbol>, Lowered)> = elems
                    .iter()
                    .map(|arg| (arg.name.clone(), self.lower_expr(&arg.value, payload_name)))
                    .collect();
                Box::new(move |ctx| {
                    Value::Tuple(
                        cs.iter_mut()
                            .map(|(name, c)| (name.clone(), c(ctx)))
                            .collect(),
                    )
                })
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                let mut cc = self.lower_expr(cond, payload_name);
                let mut tc = self.lower_expr(then, payload_name);
                let mut ec = self.lower_expr(els, payload_name);
                Box::new(move |ctx| {
                    if cc(ctx).as_bool().unwrap_or(false) {
                        tc(ctx)
                    } else {
                        ec(ctx)
                    }
                })
            }
            // Binary operators. Arithmetic (`+ - * /`, RFC-0020) promotes
            // Int↔Float; comparison (`== != < <= > >=`, RFC-0027 §1) yields a
            // `Bool`; `+` also does string/list concat (RFC-0027 §3/§4). The
            // short-circuiting `&&`/`||` (RFC-0027 §2) are lowered as control
            // flow so the un-taken side is neither evaluated nor read-tracked.
            Expr::Binary { op, lhs, rhs, .. } => {
                let op = *op;
                let mut lc = self.lower_expr(lhs, payload_name);
                let mut rc = self.lower_expr(rhs, payload_name);
                match op {
                    BinOp::And => Box::new(move |ctx| {
                        // Evaluate, and thereby read-track, the RHS only when
                        // the LHS is true (RFC-0027 §2, mirrors `when`).
                        if lc(ctx).as_bool().unwrap_or(false) {
                            Value::Bool(rc(ctx).as_bool().unwrap_or(false))
                        } else {
                            Value::Bool(false)
                        }
                    }),
                    BinOp::Or => Box::new(move |ctx| {
                        if lc(ctx).as_bool().unwrap_or(false) {
                            Value::Bool(true)
                        } else {
                            Value::Bool(rc(ctx).as_bool().unwrap_or(false))
                        }
                    }),
                    // The right side is read only when the left has nothing.
                    BinOp::Otherwise => Box::new(move |ctx| match lc(ctx) {
                        Value::Unit => rc(ctx),
                        value => value,
                    }),
                    _ => Box::new(move |ctx| eval_binary(op, lc(ctx), rc(ctx))),
                }
            }
            // Prefix unary (`!b`, `-x`), RFC-0027 §2. `!` negates a `Bool`;
            // `-` negates a numeric. A type mismatch degrades to `Unit`
            // (the checker reports it; user data never panics).
            Expr::Unary { op, rhs, .. } => {
                let op = *op;
                let mut rc = self.lower_expr(rhs, payload_name);
                Box::new(move |ctx| match (op, rc(ctx)) {
                    (UnOp::Not, Value::Bool(b)) => Value::Bool(!b),
                    (UnOp::Neg, Value::Int(n)) => Value::Int(n.wrapping_neg()),
                    (UnOp::Neg, Value::Float(f)) => Value::Float(-f),
                    _ => Value::Unit,
                })
            }
            // Indexing `base[index]` (RFC-0027 §4). Out-of-range or a
            // non-list/non-int index degrades to `Unit`, never a panic.
            Expr::Index { base, index, .. } => {
                let mut bc = self.lower_expr(base, payload_name);
                let mut ic = self.lower_expr(index, payload_name);
                Box::new(move |ctx| index_value(&bc(ctx), &ic(ctx)))
            }
            // A record literal `{ ..spread, k: v }` (RFC-0027 §6): the spread's
            // fields seed the record, then written fields append/override, all
            // keeping declaration order.
            Expr::Record { fields, spread, .. } => {
                let mut spread_c = spread.as_ref().map(|s| self.lower_expr(s, payload_name));
                let mut field_cs: Vec<(Symbol, Lowered)> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.lower_expr(v, payload_name)))
                    .collect();
                Box::new(move |ctx| {
                    let mut out: Vec<(Symbol, Value)> = match spread_c.as_mut().map(|c| c(ctx)) {
                        Some(Value::Record(fs)) => fs,
                        _ => Vec::new(),
                    };
                    for (k, c) in &mut field_cs {
                        let v = c(ctx);
                        if let Some(slot) = out.iter_mut().find(|(ek, _)| ek == k) {
                            slot.1 = v;
                        } else {
                            out.push((k.clone(), v));
                        }
                    }
                    Value::Record(out)
                })
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args, payload_name),
            // RFC-0028 §4: an async call with result arms. Only ever reached
            // through an action, `eval_pure` rejects it first, so by the time
            // it lowers here it is known to be in effect position.
            Expr::ControllerCall {
                call,
                ok,
                err,
                span,
            } => {
                let Expr::Call {
                    callee,
                    args,
                    span: call_span,
                } = call.as_ref()
                else {
                    return Box::new(|_| Value::Unit);
                };
                let _ = call_span;
                self.lower_controller_call(callee, args, ok.as_ref(), err.as_ref(), *span)
                    .unwrap_or_else(|| {
                        // Arms were written on a receiver that resolves to
                        // something other than a controller. Reported rather
                        // than lowered as a plain call: the arms would never
                        // run, and a result handler that silently never fires
                        // is the worst shape this mistake could take.
                        //
                        // A receiver that resolves to *nothing* is already
                        // covered by the `inject` diagnostic at its
                        // declaration, and saying it twice would point the
                        // developer at the call instead of the cause.
                        if self.receiver_is_bound(callee) {
                            self.errors.push(CompileError::UnknownControllerMethod {
                                span: *span,
                                controller: "this receiver".to_string(),
                                name: "an `ok`/`err` arm needs an injected controller".to_string(),
                            });
                        }
                        Box::new(|_| Value::Unit)
                    })
            }
            Expr::ClassRef(class, _) => {
                let s = format!(".{class}");
                Box::new(move |_| Value::Str(s.clone()))
            }
            Expr::Postfix { target, op, span } => {
                if let Ok(sig) = self.resolve_var(target, *span) {
                    let op = *op;
                    Box::new(move |ctx| {
                        let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                        let new = match op {
                            PostfixOp::Inc => cur + 1,
                            PostfixOp::Dec => cur - 1,
                        };
                        ctx.write_signal(sig, Value::Int(new));
                        Value::Unit
                    })
                } else {
                    Box::new(|_| Value::Unit)
                }
            }
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                if let Ok(sig) = self.resolve_var(target, *span) {
                    let op = *op;
                    let mut rhs = self.lower_expr(value, payload_name);
                    // A `var` of a record type: what arrives is compared with
                    // the type, and the first difference is reported, once
                    // for this assignment.
                    let shape = self.var_types.get(&sig).cloned().map(|ty| {
                        (
                            ty,
                            std::rc::Rc::clone(&self.records),
                            std::rc::Rc::clone(&self.bridge.diagnostics),
                            std::rc::Rc::clone(&self.shapes_told),
                            target_name(target),
                            *span,
                        )
                    });
                    Box::new(move |ctx| {
                        let val = rhs(ctx);
                        let new = match op {
                            AssignOp::Assign => val,
                            AssignOp::Add => {
                                let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                                Value::Int(cur + val.as_int().unwrap_or(0))
                            }
                            AssignOp::Sub => {
                                let cur = ctx.peek_signal(sig).as_int().unwrap_or(0);
                                Value::Int(cur - val.as_int().unwrap_or(0))
                            }
                        };
                        if let Some((ty, records, diagnostics, told, var, span)) = &shape {
                            let at = (span.start, span.end);
                            if !told.borrow().contains(&at) {
                                if let Some(problem) = super::shape::difference(&new, ty, records) {
                                    told.borrow_mut().insert(at);
                                    diagnostics.borrow_mut().push(CompileError::ShapeMismatch {
                                        span: *span,
                                        var: var.clone(),
                                        expected: super::shape::written(ty),
                                        problem,
                                    });
                                }
                            }
                        }
                        ctx.write_signal(sig, new);
                        Value::Unit
                    })
                } else {
                    Box::new(|_| Value::Unit)
                }
            }
            // A `style { … }` value (RFC-0016) is consumed structurally, bound
            // via `let` into the style table and spliced by `..` at lower time
            // (see `register_style`/`expand_style_spreads`), never projected as
            // a scalar. Reaching here means it was used where a value was
            // expected, which has no meaning; yield Unit.
            Expr::StyleValue { .. } | Expr::Merge { .. } => Box::new(|_| Value::Unit),
            // `value with anim.*(…)` (RFC-0010): lower to the *target* value.
            // The curve is validated by the checker; the `Motion` runtime that
            // actually drives the on-screen transition lands in the follow-up
            // slice, so for now the target resolves instantly (as it did before
            // any `with` was written), which is a safe, correct fallback.
            Expr::Animated { value, .. } => self.lower_expr(value, payload_name),
            // A keyframe step (RFC-0025) only means anything inside
            // `anim.keyframes(…)`, which is driven at the `eval_pure`
            // chokepoint. Reaching the reactive lowering path means it was
            // written somewhere else; lower to its value, the same inert
            // fallback `Animated` takes.
            #[allow(clippy::match_same_arms)] // a different rule, same fallback
            Expr::KeyframeStep { value, .. } => self.lower_expr(value, payload_name),
            // A callback-prop action block (RFC-0019): lower each statement in
            // order and run them in sequence when the callback fires, returning
            // the last statement's value (`Unit` for the no-op default `{}`).
            // Mutations inside route through the reactive system exactly like any
            // event-handler action, because each `Assign`/`Postfix` statement
            // lowers to a signal write.
            //
            // A `let` holds its value for the statements after it: its name is
            // read through the same stack a lambda's parameters are, pushed
            // when the statement runs and popped when the block ends.
            Expr::Block(stmts, _) => {
                let shadowed = self.lambda_params.len();
                let mut cs: Vec<(Option<Symbol>, Lowered)> = Vec::with_capacity(stmts.len());
                for stmt in stmts {
                    if let Expr::Let { name, .. } = stmt {
                        let c = self.lower_expr(stmt, payload_name);
                        self.lambda_params.push(name.clone());
                        cs.push((Some(name.clone()), c));
                    } else {
                        cs.push((None, self.lower_expr(stmt, payload_name)));
                    }
                }
                self.lambda_params.truncate(shadowed);
                Box::new(move |ctx| {
                    let base = LAMBDA_ARGS.with(|args| args.borrow().len());
                    let mut last = Value::Unit;
                    for (name, c) in &mut cs {
                        let value = c(ctx);
                        if let Some(name) = name {
                            LAMBDA_ARGS.with(|args| args.borrow_mut().push((name.clone(), value)));
                            last = Value::Unit;
                        } else {
                            last = value;
                        }
                    }
                    LAMBDA_ARGS.with(|args| args.borrow_mut().truncate(base));
                    last
                })
            }
            // A `let`'s value. Of a record type, what arrives is compared with
            // the type and the first difference reported, once, as for a
            // `var` of one.
            Expr::Let {
                name,
                ty,
                value,
                span,
            } => {
                let mut compute = self.lower_expr(value, payload_name);
                let Some(ty) = ty
                    .clone()
                    .filter(|t| super::shape::names_a_record(t, &self.records))
                else {
                    return compute;
                };
                let records = std::rc::Rc::clone(&self.records);
                let diagnostics = std::rc::Rc::clone(&self.bridge.diagnostics);
                let told = std::rc::Rc::clone(&self.shapes_told);
                let (name, span) = (name.as_str().to_string(), *span);
                Box::new(move |ctx| {
                    let value = compute(ctx);
                    let at = (span.start, span.end);
                    if !told.borrow().contains(&at) {
                        if let Some(problem) = super::shape::difference(&value, &ty, &records) {
                            told.borrow_mut().insert(at);
                            diagnostics.borrow_mut().push(CompileError::ShapeMismatch {
                                span,
                                var: name.clone(),
                                expected: super::shape::written(&ty),
                                problem,
                            });
                        }
                    }
                    value
                })
            }
            // An `if` statement: only the taken branch runs, so a guard's write
            // and the reads inside the branch not taken never happen.
            Expr::If {
                cond, then, els, ..
            } => {
                let mut cc = self.lower_expr(cond, payload_name);
                let mut tc = self.lower_expr(then, payload_name);
                let mut ec = els.as_deref().map(|e| self.lower_expr(e, payload_name));
                Box::new(move |ctx| {
                    if cc(ctx).as_bool().unwrap_or(false) {
                        tc(ctx)
                    } else {
                        ec.as_mut().map_or(Value::Unit, |ec| ec(ctx))
                    }
                })
            }
            // A `theme.<token>` access (RFC-0022): reads the reactive scheme
            // signal and projects the token's value for the active scheme. Any
            // other member access needs controller metadata (not modeled in
            // Phase 2); lambdas/assignments are actions, not projected values.
            Expr::Member { base, field, span } => {
                if let Some(lowered) = self.lower_theme_member(base, field, *span) {
                    return lowered;
                }
                // Data member access (RFC-0027 §4/§6): `xs.len` (list length) and
                // `r.field` (record field). Unknown members degrade to `Unit`;
                // the checker reports genuinely unknown ones.
                let field = field.clone();
                let mut base_c = self.lower_expr(base, payload_name);
                Box::new(move |ctx| data_member(&base_c(ctx), &field))
            }
            Expr::Lambda { .. } | Expr::Error(_) => Box::new(|_| Value::Unit),
        }
    }

    /// Lowers a `theme.<field>` access to a reactive projection (RFC-0022 §1),
    /// or returns `None` when `base` does not resolve to an injected
    /// [`Value::Theme`] (leaving the caller to fall back to `Unit`).
    ///
    /// The returned closure reads the active-scheme signal *tracked*, so any
    /// binding that projects a token re-runs when the scheme flips. Token data
    /// is resolved once, here, and captured by value, the closure never borrows
    /// the interpreter.
    fn lower_theme_member(&mut self, base: &Expr, field: &Symbol, span: Span) -> Option<Lowered> {
        let Expr::Ident(base_name, _) = base else {
            return None;
        };
        let sig = match self.env.lookup(base_name) {
            Some(Value::Theme(sig)) => *sig,
            _ => return None,
        };
        let f = field.as_str();

        // Reserved reactive members: the scheme flag itself.
        if f == "dark" {
            return Some(Box::new(move |ctx| ctx.read_signal(sig)));
        }
        if f == "mode" {
            return Some(Box::new(move |ctx| {
                let dark = ctx.read_signal(sig).as_bool().unwrap_or(false);
                Value::Str(if dark { "dark" } else { "light" }.to_string())
            }));
        }

        // Color tokens differ per scheme → capture both resolved values.
        let light = self.theme.color(f, false);
        let dark = self.theme.color(f, true);
        if light.is_some() || dark.is_some() {
            let mix_sig = self.theme_mix;
            return Some(Box::new(move |ctx| {
                let is_dark = ctx.read_signal(sig).as_bool().unwrap_or(false);
                let (current, previous) = if is_dark {
                    (dark.or(light), light.or(dark))
                } else {
                    (light.or(dark), dark.or(light))
                };
                let current = current.unwrap_or(0);
                // RFC-0016 animated token transitions: while a flip is under
                // way the token reads a blend from the scheme it is leaving.
                // At rest (`mix == 1`) it returns exactly the value it always
                // did, not a blend that happens to land on it, so a theme with
                // no transition is byte-identical to one written before this.
                #[allow(clippy::cast_possible_truncation)]
                let mix = match mix_sig.map(|m| ctx.read_signal(m)) {
                    Some(Value::Float(m)) => m as f32,
                    _ => 1.0,
                };
                if mix >= 1.0 {
                    return Value::Int(current);
                }
                // Smoothstep, which is symmetric about one half: reversing a
                // flip half way through replaces `mix` with `1 - mix`, and the
                // eased value is continuous across that, so a second tap never
                // jumps.
                let t = mix.clamp(0.0, 1.0);
                let eased = t * t * (3.0 - 2.0 * t);
                Value::Int(mix_hex_oklab(previous.unwrap_or(current), current, eased))
            }));
        }

        // Typography tokens: project the size (the current `typo:`/`size:`
        // pipeline is size-only; weight/family land with font byte-loading).
        if let Some(size) = self.theme.typo_size(f) {
            #[allow(clippy::cast_possible_truncation)]
            let size = size as i64;
            // Still read the signal so the binding is theme-scoped and re-runs on
            // a scheme flip (typography can differ per scheme in future themes).
            return Some(Box::new(move |ctx| {
                let _ = ctx.read_signal(sig);
                Value::Int(size)
            }));
        }

        // Shape (corner-radius) tokens.
        if let Some(radius) = self.theme.shape(f) {
            #[allow(clippy::cast_possible_truncation)]
            let radius = radius.round() as i64;
            return Some(Box::new(move |ctx| {
                let _ = ctx.read_signal(sig);
                Value::Int(radius)
            }));
        }

        // A member of a theme that names no known token is a hard error.
        self.errors.push(CompileError::UnknownThemeToken {
            span,
            field: f.to_string(),
            theme: self.theme.name.clone(),
        });
        Some(Box::new(|_| Value::Unit))
    }

    fn lower_ident(&self, name: &Symbol, payload_name: Option<&Symbol>) -> Lowered {
        if let Some(pname) = payload_name {
            if pname == name {
                return Box::new(move |_| {
                    CURRENT_PAYLOAD.with(|cell| cell.borrow().clone().unwrap_or(Value::Unit))
                });
            }
        }
        // After the single-parameter slot, so a nested `map(x => …)` inside a
        // `reduce(…, (acc, x) => …)` reads its own `x`, the innermost binding.
        if self.lambda_params.contains(name) {
            let name = name.clone();
            return Box::new(move |_| {
                LAMBDA_ARGS.with(|args| {
                    args.borrow()
                        .iter()
                        .rev()
                        .find(|(n, _)| *n == name)
                        .map_or(Value::Unit, |(_, v)| v.clone())
                })
            });
        }
        match name.as_str() {
            "true" => return Box::new(|_| Value::Bool(true)),
            "false" => return Box::new(|_| Value::Bool(false)),
            _ => {}
        }
        match self.env.lookup(name) {
            Some(Value::Signal(sig)) => {
                let sig = *sig;
                Box::new(move |ctx| ctx.read_signal(sig))
            }
            Some(Value::Memo(scope)) => {
                let m = *scope;
                Box::new(move |ctx| ctx.read_memo(m))
            }
            Some(v) => {
                let v = v.clone();
                Box::new(move |_| v.clone())
            }
            // An unresolved identifier is treated as an enum/style token
            // (e.g. `center`, `cover`); intrinsics validate it.
            None => {
                let token = name.as_str().to_string();
                Box::new(move |_| Value::Str(token.clone()))
            }
        }
    }

    fn lower_strlit(&mut self, parts: &[StrPart], payload_name: Option<&Symbol>) -> Lowered {
        enum Part {
            Text(String),
            Interp(Lowered, Option<u8>),
        }
        let mut lowered: Vec<Part> = parts
            .iter()
            .map(|p| match p {
                StrPart::Text(t) => Part::Text(t.clone()),
                StrPart::Interp(e, decimals) => {
                    Part::Interp(self.lower_expr(e, payload_name), *decimals)
                }
            })
            .collect();
        Box::new(move |ctx| {
            let mut s = String::new();
            for part in &mut lowered {
                match part {
                    Part::Text(t) => s.push_str(t),
                    Part::Interp(c, None) => s.push_str(&format_scalar(&c(ctx))),
                    Part::Interp(c, Some(n)) => s.push_str(&format_fixed(&c(ctx), *n)),
                }
            }
            Value::Str(s)
        })
    }

    fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[crate::parser::ast::Arg],
        payload_name: Option<&Symbol>,
    ) -> Lowered {
        // RFC-0026 `navigate`/`back`/`replace`. Contextual: the names are only
        // special when their first argument really is a navigation container's
        // `var`, so nothing stops an app binding them for its own purposes.
        if let Some(lowered) = self.lower_nav_action(callee, args, payload_name) {
            return lowered;
        }
        // `untrack(expr)`, the reserved escape hatch (D2).
        if let Expr::Ident(name, _) = callee {
            if name.as_str() == "untrack" {
                if let Some(arg) = args.first() {
                    let mut inner = self.lower_expr(&arg.value, payload_name);
                    return Box::new(move |ctx| untrack(|| inner(ctx)));
                }
            }
            // A zero-arg call to a `fn`/`let` memo reads that memo.
            if let Some(Value::Memo(scope)) = self.env.lookup(name) {
                let m = *scope;
                return Box::new(move |ctx| ctx.read_memo(m));
            }
            // Parameterized fn call *or* callback-prop invocation
            // (RFC-0019 §3): inline the body with args bound as memos. For a
            // callback, the body is the *caller's* action block, still resolved
            // here, where the caller's `var`s remain live below the callee frame
            // in the shared flat env, so `count++` routes to the caller's signal.
            // A top-level function, when nothing in the view has the name.
            if self.env.lookup(name).is_none() {
                if let Some(&id) = self.top_fns.get(name) {
                    return self.inline_top_fn(name, id, callee.span(), args, payload_name);
                }
            }
            if let Some(Value::Fn(id)) = self.env.lookup(name).cloned() {
                if (id.0 as usize) < self.fn_table.len() {
                    let (params, body, is_callback) = self.fn_table[id.0 as usize].clone();
                    // A callback invoked with the wrong arity is a hard error
                    // (RFC-0019 §4); a plain `fn` keeps the historical lenient
                    // zip (extra args ignored, missing bound to nothing).
                    if is_callback && params.len() != args.len() {
                        self.errors.push(CompileError::CallbackArityMismatch {
                            span: callee.span(),
                            name: name.as_str().to_string(),
                            expected: params.len(),
                            found: args.len(),
                        });
                    }
                    // Bind each arg as a reactive memo so signal reads inside the
                    // body are tracked by the enclosing scope.
                    let snapshot = self.env.len();
                    let inlined = self.inlined_params.len();
                    for (param, arg) in params.iter().zip(args.iter()) {
                        let arg_lowered = self.lower_expr(&arg.value, payload_name);
                        let scope = self.ctx.open_memo(arg_lowered);
                        self.env.push(param.clone(), Value::Memo(scope));
                        self.inlined_params.push((param.clone(), scope));
                    }
                    // Lower the body with arg bindings in scope.
                    let body_lowered = self.lower_expr(&body, payload_name);
                    // Restore env.
                    self.env.truncate(snapshot);
                    self.inlined_params.truncate(inlined);
                    return body_lowered;
                }
            }
        }
        // Collection method calls (RFC-0027 §4): `xs.push(v)`, `.removeAt(i)`,
        // `.contains(v)`, `.map(f)`, `.filter(f)`, each pure and value-returning.
        if let Expr::Member { base, field, .. } = callee {
            if let Some(lowered) = self.lower_collection_method(base, field, args, payload_name) {
                return lowered;
            }
        }
        // A call on an injected controller handle with no arms: fire-and-forget
        // (RFC-0028 §4). It takes the *same* lowering as the arm-bearing form,
        // so whether anyone reads the answer cannot change what the call does.
        if let Some(lowered) = self.lower_controller_call(callee, args, None, None, callee.span()) {
            return lowered;
        }
        Box::new(|_| Value::Unit)
    }

    /// Inlines a call to a top-level function. Its arguments are lowered in
    /// the caller's environment; its body in an environment holding only its
    /// parameters, so it reads nothing of the view that calls it (a function
    /// shared by every view cannot depend on which one called it).
    fn inline_top_fn(
        &mut self,
        name: &Symbol,
        id: super::env::AstId,
        span: Span,
        args: &[crate::parser::ast::Arg],
        payload_name: Option<&Symbol>,
    ) -> Lowered {
        if self.inlining.contains(name) {
            self.errors.push(CompileError::RecursiveFunction {
                span,
                name: name.as_str().to_string(),
            });
            return Box::new(|_| Value::Unit);
        }
        let (params, body, _) = self.fn_table[id.0 as usize].clone();
        let mut bound = Vec::with_capacity(params.len());
        for (param, arg) in params.iter().zip(args.iter()) {
            let arg_lowered = self.lower_expr(&arg.value, payload_name);
            bound.push((param.clone(), self.ctx.open_memo(arg_lowered)));
        }
        let caller = std::mem::replace(&mut self.env, Env::new());
        for (param, scope) in bound {
            self.env.push(param, Value::Memo(scope));
        }
        self.inlining.push(name.clone());
        let body_lowered = self.lower_expr(&body, None);
        self.inlining.pop();
        self.env = caller;
        body_lowered
    }

    /// Lowers a collection method call (RFC-0027 §4). Returns `None` for a
    /// non-collection method name so `anim.*` curve calls and other member calls
    /// keep their existing (non-data) handling.
    fn lower_collection_method(
        &mut self,
        base: &Expr,
        name: &Symbol,
        args: &[crate::parser::ast::Arg],
        payload_name: Option<&Symbol>,
    ) -> Option<Lowered> {
        let mut base_c = self.lower_expr(base, payload_name);
        match name.as_str() {
            "push" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let v = arg(ctx);
                    match base_c(ctx) {
                        Value::List(mut xs) => {
                            xs.push(v);
                            Value::List(xs)
                        }
                        other => other,
                    }
                }))
            }
            "removeAt" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let i = arg(ctx).as_int().and_then(|i| usize::try_from(i).ok());
                    match base_c(ctx) {
                        Value::List(mut xs) => {
                            // Out-of-range → unchanged list (no panic on user data).
                            if let Some(i) = i.filter(|i| *i < xs.len()) {
                                xs.remove(i);
                            }
                            Value::List(xs)
                        }
                        other => other,
                    }
                }))
            }
            "slice" => {
                // `xs.slice(start)` or `xs.slice(start, end)`, end exclusive,
                // on a list or on text (by character). Out-of-range bounds
                // clamp: user data never panics.
                let mut start = self.lower_expr(&args.first()?.value, payload_name);
                let mut end = args.get(1).map(|a| self.lower_expr(&a.value, payload_name));
                Some(Box::new(move |ctx| {
                    let from = start(ctx).as_int();
                    let to = end.as_mut().map(|e| e(ctx).as_int());
                    let range = |len: usize| {
                        let clamp = |i: Option<i64>, default: usize| {
                            i.map_or(default, |i| {
                                usize::try_from(i.max(0)).unwrap_or(len).min(len)
                            })
                        };
                        let a = clamp(from, 0);
                        let b = to.map_or(len, |t| clamp(t, len));
                        a..b.max(a)
                    };
                    match base_c(ctx) {
                        Value::List(xs) => Value::List(xs[range(xs.len())].to_vec()),
                        Value::Str(s) => {
                            let r = range(s.chars().count());
                            Value::Str(s.chars().skip(r.start).take(r.len()).collect())
                        }
                        other => other,
                    }
                }))
            }
            // `text.urlEncoded()`: text made safe to put in a URL, as one
            // query value or one path segment.
            // `color.alpha(0.6)`: the colour at that opacity, so a theme token
            // gives its own translucent tints (`t.secondary.alpha(0.8)`)
            // instead of a hex written out beside it.
            "alpha" => {
                let mut amount = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let amount = match amount(ctx) {
                        Value::Float(f) => f,
                        #[allow(clippy::cast_precision_loss)]
                        Value::Int(n) => n as f64,
                        _ => return Value::Unit,
                    };
                    match base_c(ctx) {
                        Value::Int(color) => Value::Int(with_alpha(color, amount)),
                        _ => Value::Unit,
                    }
                }))
            }
            "urlEncoded" => Some(Box::new(move |ctx| match base_c(ctx) {
                Value::Str(s) => Value::Str(url_encoded(&s)),
                other => other,
            })),
            // `iso.formatDate("ddd D MMM")`: an ISO date written out, or
            // nothing when the text is not one (so `?? "—"` reads naturally).
            "formatDate" => {
                let mut pattern = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let pattern = pattern(ctx);
                    match (base_c(ctx), pattern) {
                        (Value::Str(text), Value::Str(pattern)) => {
                            super::dates::format_date(&text, &pattern)
                                .map_or(Value::Unit, Value::Str)
                        }
                        _ => Value::Unit,
                    }
                }))
            }
            "contains" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let needle = arg(ctx);
                    match base_c(ctx) {
                        Value::List(xs) => {
                            Value::Bool(xs.iter().any(|x| structural_eq(x, &needle)))
                        }
                        _ => Value::Bool(false),
                    }
                }))
            }
            "sort" => Some(Box::new(move |ctx| match base_c(ctx) {
                Value::List(mut xs) => {
                    xs.sort_by(value_order);
                    Value::List(xs)
                }
                other => other,
            })),
            "min" | "max" => {
                let is_max = name.as_str() == "max";
                Some(Box::new(move |ctx| match base_c(ctx) {
                    Value::List(xs) => {
                        let pick = if is_max {
                            xs.into_iter().max_by(value_order)
                        } else {
                            xs.into_iter().min_by(value_order)
                        };
                        pick.unwrap_or(Value::Unit)
                    }
                    _ => Value::Unit,
                }))
            }
            "indexOf" => {
                let mut arg = self.lower_expr(&args.first()?.value, payload_name);
                Some(Box::new(move |ctx| {
                    let needle = arg(ctx);
                    let found = match base_c(ctx) {
                        Value::List(xs) => xs.iter().position(|x| structural_eq(x, &needle)),
                        _ => None,
                    };
                    Value::Int(found.and_then(|i| i64::try_from(i).ok()).unwrap_or(-1))
                }))
            }
            "sortBy" | "find" => {
                let (param, body) = match &args.first()?.value {
                    Expr::Lambda { params, body, .. } => (params.first().cloned(), body.clone()),
                    _ => return None,
                };
                let mut body_c = self.lower_expr(&body, param.as_ref());
                let is_find = name.as_str() == "find";
                Some(Box::new(move |ctx| {
                    let Value::List(xs) = base_c(ctx) else {
                        return if is_find {
                            Value::Unit
                        } else {
                            Value::List(Vec::new())
                        };
                    };
                    if is_find {
                        return xs
                            .into_iter()
                            .find(|x| {
                                with_lambda_elem(x.clone(), || body_c(ctx))
                                    .as_bool()
                                    .unwrap_or(false)
                            })
                            .unwrap_or(Value::Unit);
                    }
                    // Each key computed once, then a stable sort by it.
                    let mut keyed: Vec<(Value, Value)> = xs
                        .into_iter()
                        .map(|x| (with_lambda_elem(x.clone(), || body_c(ctx)), x))
                        .collect();
                    keyed.sort_by(|a, b| value_order(&a.0, &b.0));
                    Value::List(keyed.into_iter().map(|(_, x)| x).collect())
                }))
            }
            "reduce" => {
                // `xs.reduce(init, (acc, x) => body)`: two parameters, bound by
                // name through `LAMBDA_ARGS` while the body is lowered.
                let mut init = self.lower_expr(&args.first()?.value, payload_name);
                let (params, body) = match &args.get(1)?.value {
                    Expr::Lambda { params, body, .. } if params.len() == 2 => {
                        (params.clone(), body.clone())
                    }
                    _ => return None,
                };
                let mark = self.lambda_params.len();
                self.lambda_params.extend(params.iter().cloned());
                let mut body_c = self.lower_expr(&body, payload_name);
                self.lambda_params.truncate(mark);
                let (acc_name, elem_name) = (params[0].clone(), params[1].clone());
                Some(Box::new(move |ctx| {
                    let mut acc = init(ctx);
                    let Value::List(xs) = base_c(ctx) else {
                        return acc;
                    };
                    for x in xs {
                        let base = LAMBDA_ARGS.with(|a| {
                            let mut a = a.borrow_mut();
                            let base = a.len();
                            a.push((acc_name.clone(), acc.clone()));
                            a.push((elem_name.clone(), x));
                            base
                        });
                        acc = body_c(ctx);
                        LAMBDA_ARGS.with(|a| a.borrow_mut().truncate(base));
                    }
                    acc
                }))
            }
            "map" | "filter" => {
                // The predicate/transform must be a lambda (RFC-0027 §5). Lower
                // its body once with the parameter routed through the per-element
                // slot (like an event payload), evaluated per element below.
                let (param, body) = match &args.first()?.value {
                    Expr::Lambda { params, body, .. } => (params.first().cloned(), body.clone()),
                    _ => return None,
                };
                let mut body_c = self.lower_expr(&body, param.as_ref());
                let is_map = name.as_str() == "map";
                Some(Box::new(move |ctx| {
                    let Value::List(xs) = base_c(ctx) else {
                        return Value::List(Vec::new());
                    };
                    let mut out = Vec::with_capacity(xs.len());
                    for elem in xs {
                        let mapped = with_lambda_elem(elem.clone(), || body_c(ctx));
                        if is_map {
                            out.push(mapped);
                        } else if mapped.as_bool().unwrap_or(false) {
                            out.push(elem);
                        }
                    }
                    Value::List(out)
                }))
            }
            _ => None,
        }
    }

    // ── actions (mutations & bare expressions) ──────────────────────────

    /// Evaluates an expression with no reactive scope active (an *action*, not a
    /// projection). Mutations route through the mark cascade; a mutation on a
    /// non-`var` l-value is [`CompileError::NotAssignable`].
    ///
    /// # Errors
    ///
    /// Returns [`CompileError::NotAssignable`] if a mutation targets something
    /// other than a `var`.
    pub fn eval_action(&mut self, expr: &Expr) -> Result<Value, CompileError> {
        match expr {
            Expr::Postfix { target, op, span } => {
                let sig = self.resolve_var(target, *span)?;
                let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                let new = match op {
                    PostfixOp::Inc => cur + 1,
                    PostfixOp::Dec => cur - 1,
                };
                self.ctx.write_signal(sig, Value::Int(new));
                Ok(Value::Unit)
            }
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                let sig = self.resolve_var(target, *span)?;
                let rhs = self.eval_pure(value);
                let new = match op {
                    AssignOp::Assign => rhs,
                    AssignOp::Add => {
                        let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                        Value::Int(cur + rhs.as_int().unwrap_or(0))
                    }
                    AssignOp::Sub => {
                        let cur = self.ctx.peek_signal(sig).as_int().unwrap_or(0);
                        Value::Int(cur - rhs.as_int().unwrap_or(0))
                    }
                };
                self.ctx.write_signal(sig, new);
                Ok(Value::Unit)
            }
            // A brace action `=> { stmt* }` parses as a zero-parameter lambda
            // over a block (RFC-0019). Unwrap it and run each statement as an
            // action in order, so a multi-statement handler (e.g. the todo
            // "Add": `todos = todos.push(…)` then `draft = ""`) writes every
            // `var` it names, not just the last expression's value.
            Expr::Lambda { params, body, .. } if params.is_empty() => self.eval_action(body),
            Expr::Block(stmts, _) => {
                let mut last = Value::Unit;
                for stmt in stmts {
                    last = self.eval_action(stmt)?;
                }
                Ok(last)
            }
            Expr::If {
                cond, then, els, ..
            } => {
                if self.eval_pure(cond).as_bool().unwrap_or(false) {
                    self.eval_action(then)
                } else if let Some(els) = els {
                    self.eval_action(els)
                } else {
                    Ok(Value::Unit)
                }
            }
            other => Ok(self.eval_pure(other)),
        }
    }

    /// Evaluates `expr` once, immediately, with no scope active (so nothing
    /// subscribes). Used to seed `var`s and to evaluate action operands.
    pub(super) fn eval_pure(&mut self, expr: &Expr) -> Value {
        // RFC-0028 §4: a call is an effect. `eval_pure` is every place a value
        // is *needed*, a `var` seed, an attribute, a memo body, so rejecting
        // it here rejects it in all of them at once, rather than in each of
        // them separately and eventually in all but one.
        if let Expr::ControllerCall { span, .. } = expr {
            self.errors.push(CompileError::EffectInPureContext {
                span: *span,
                context: "a value position".to_string(),
            });
            return Value::Unit;
        }
        // A `with` animation (RFC-0010) is driven here, at the single evaluation
        // chokepoint, so every animatable scalar prop (opacity/scale/translate/
        // rotate, all of which resolve through `eval_pure`) animates without
        // per-prop plumbing. A non-animated value takes the ordinary path.
        if let Expr::Animated { value, anim, span } = expr {
            return self.eval_animated(value, anim, self.anim_key(*span));
        }
        // An `anim.keyframes(…)` sequence (RFC-0025 §3) *is* the property value,
        // so it is driven at the same chokepoint: every animatable scalar (and
        // the coordinate pairs, which interpolate component-wise) gets keyframes
        // for free, with no per-prop plumbing.
        if crate::interp::anim::is_keyframes_call(expr) {
            return self.eval_keyframes(expr);
        }
        if let Some(value) = numeric_literal(expr) {
            return value;
        }
        // A tuple of numeric literals (`translate: (40, 0)`, an animation's
        // target or `from:`) is the commonest animated value. Lowering it would
        // box a closure per component only to call each once, so it is built
        // directly, the same value `lower_expr` would produce.
        if let Expr::Tuple(args, _) = expr {
            if args.iter().all(|arg| numeric_literal(&arg.value).is_some()) {
                return Value::Tuple(
                    args.iter()
                        .map(|arg| {
                            let value = numeric_literal(&arg.value).unwrap_or(Value::Unit);
                            (arg.name.clone(), value)
                        })
                        .collect(),
                );
            }
        }
        let mut compute = self.lower_expr(expr, None);
        compute(&mut self.ctx)
    }

    /// Evaluates `expr` as an `f32`, falling back to `default` for a
    /// non-numeric result (the checker restricts these positions to numbers).
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub(super) fn eval_number(&mut self, expr: &Expr, default: f32) -> f32 {
        match self.eval_pure(expr) {
            Value::Float(f) => f as f32,
            Value::Int(n) => n as f32,
            _ => default,
        }
    }

    fn resolve_var(&self, target: &Expr, span: Span) -> Result<SignalId, CompileError> {
        if let Expr::Ident(name, _) = target {
            if let Some(Value::Signal(sig)) = self.env.lookup(name) {
                return Ok(*sig);
            }
        }
        // `theme.dark = …` writes the reactive scheme signal (RFC-0022 §1), so a
        // scheme flip drives Mark-and-Pull across every token reference.
        if let Some(sig) = self.resolve_theme_scheme_target(target) {
            return Ok(sig);
        }
        Err(CompileError::NotAssignable { span })
    }

    /// Lowers an action expression to an event handler closure, capturing any optional payload bindings.
    ///
    /// # Errors
    ///
    /// Returns a [`CompileError`] if variable resolution or assignment validation fails.
    #[allow(clippy::needless_pass_by_value)]
    pub fn lower_action(
        &mut self,
        expr: &Expr,
        payload_name: Option<Symbol>,
    ) -> Result<Action, CompileError> {
        // A brace action `=> { stmt* }` parses as a zero-parameter lambda over a
        // block (RFC-0019). Lower the block body directly, its statements each
        // lower to a signal write, so a multi-statement handler runs every
        // statement, not the inert lambda value (which lowers to `Unit`).
        let expr = match expr {
            Expr::Lambda { params, body, .. } if params.is_empty() => body.as_ref(),
            other => other,
        };
        let mut compute = self.in_action_position(|s| s.lower_expr(expr, payload_name.as_ref()));
        Ok(Box::new(move |ctx, payload| {
            CURRENT_PAYLOAD.with(|cell| {
                *cell.borrow_mut() = payload.cloned();
            });
            let _ = compute(ctx);
            CURRENT_PAYLOAD.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }))
    }
}
