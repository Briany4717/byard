//! Fonts, text measurement and the theme, on the interpreter.
//!
//! Everything that decides which face and size a piece of text is shaped in
//! lives here (the `weight:`, `font:` and `typo:` properties, RFC-0034, and the
//! check that every named family was declared), alongside the active theme:
//! installing it, flipping its colour scheme and the cross-fade between the
//! two (RFC-0022).

use super::Interpreter;
use crate::diagnostics::CompileError;
use crate::interp::env::{SignalId, Value};
use crate::parser::ast::{Attr, AttrKind, ElementNode, Expr, Member, ViewDecl};
use crate::symbol::Symbol;

impl Interpreter {
    /// Moves the colour tokens' cross-fade on by the time since it last moved,
    /// and restarts it when the scheme has flipped (RFC-0016 animated token
    /// transitions).
    ///
    /// One mix for the whole theme rather than one animation per token, still
    /// less one per element: every token is heading from the same scheme to the
    /// same scheme, so a single number describes all of them, and a theme with
    /// forty tokens flips for the price of one.
    ///
    /// A flip half way through a transition does not restart from the far end.
    /// The appearance at `mix` is the same colour as the reverse blend at
    /// `1 - mix`, so the mix is simply replaced by that and the picture
    /// continues from where it was.
    pub(super) fn advance_theme_mix(&mut self) {
        let (Some(scheme), Some(mix_sig)) = (self.theme_scheme, self.theme_mix) else {
            return;
        };
        let dark = self.ctx.peek_signal(scheme).as_bool().unwrap_or(false);
        let duration = self.theme.transition_ms;
        match self.theme_mix_dark {
            None => {
                // The first scheme a view is shown in is not a flip.
                self.theme_mix_dark = Some(dark);
                return;
            }
            Some(previous) if previous != dark => {
                self.theme_mix_dark = Some(dark);
                // No transition configured, or no clock to run one on: the
                // cut, exactly as before.
                if duration == 0 || !self.clock_set {
                    return;
                }
                self.theme_mix_value = 1.0 - self.theme_mix_value;
                self.theme_mix_at = Some(self.now_ms);
                self.ctx
                    .write_signal(mix_sig, Value::Float(f64::from(self.theme_mix_value)));
                return;
            }
            Some(_) => {}
        }
        if self.theme_mix_value >= 1.0 || duration == 0 {
            return;
        }
        let at = self.theme_mix_at.unwrap_or(self.now_ms);
        #[allow(clippy::cast_precision_loss)]
        let step = self.now_ms.saturating_sub(at) as f32 / duration as f32;
        self.theme_mix_at = Some(self.now_ms);
        if step <= 0.0 {
            return;
        }
        self.theme_mix_value = (self.theme_mix_value + step).min(1.0);
        self.ctx
            .write_signal(mix_sig, Value::Float(f64::from(self.theme_mix_value)));
    }

    /// Whether the colour tokens are part way through a scheme flip.
    ///
    /// Asked of the signal's existence first: the fields default to zero, and
    /// an interpreter with no theme reading "zero of the way through" would
    /// keep the runner awake for ever.
    pub(super) fn theme_transitioning(&self) -> bool {
        self.theme_mix.is_some() && self.theme_mix_value < 1.0
    }

    /// Glyph-accurate `(width, height)` of `text` at `font_size`, lazily
    /// initializing the font system on first use.
    pub(super) fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        self.measure_text_wrapped(text, font_size, None, 400, None)
    }

    /// Measures `text`, wrapping to `max_width` logical pixels when `Some`
    /// (RFC-0018 text wrap). Returns the wrapped `(width, height)`.
    pub(super) fn measure_text_wrapped(
        &mut self,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
        weight: u16,
        family: Option<&str>,
    ) -> (f32, f32) {
        self.text_measurer
            .borrow_mut()
            .get_or_insert_with(byard_core::text::TextMeasurer::new)
            .measure_wrapped(text, font_size, max_width, weight, family)
    }

    /// The `weight:` of a text-bearing element on the CSS axis (RFC-0034).
    ///
    /// Accepts the four historical keywords and any integer `100..=900`; the
    /// keywords are aliases for `100/400/500/700`, so both spellings land on
    /// the same axis rather than on two parallel notions of weight. Absent is
    /// `400`.
    pub(super) fn resolve_weight(&mut self, attrs: &[Attr]) -> u16 {
        // An explicit `weight:` always wins. Failing that, a `typo:` token
        // brings its own — a theme that calls its headline semibold means it,
        // and the token's weight was being dropped on the way through while
        // only its size survived.
        if !attrs.iter().any(|a| a.name.as_str() == "weight") {
            if let Some(w) = self.typo_weight(attrs) {
                return w;
            }
            return 400;
        }
        if let Some(kw) = Self::enum_prop(attrs, "weight") {
            return match kw {
                "thin" => 100,
                "medium" => 500,
                "bold" => 700,
                _ => 400,
            };
        }
        match self.eval_px_prop(attrs, "weight") {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(n) => (n.round().clamp(100.0, 900.0)) as u16,
            None => 400,
        }
    }

    /// The attributes that describe *type* rather than *box*, copied so a
    /// wrapper element can hand them to the text it contains.
    ///
    /// The list is exactly the text properties the intrinsic catalogue puts on
    /// a text-bearing widget. A widget that wraps its label in a box has to
    /// forward them or accept them into a void.
    pub(super) fn text_shaping_attrs(attrs: &[Attr]) -> Vec<Attr> {
        const TYPE_PROPS: &[&str] = &[
            "typo", "size", "weight", "font", "color", "align", "wrap", "lines",
        ];
        attrs
            .iter()
            .filter(|a| TYPE_PROPS.contains(&a.name.as_str()))
            .cloned()
            .collect()
    }

    /// The resolved font family a text-bearing element shapes in (RFC-0034).
    ///
    /// Three layers, in this order, and only the first can be wrong:
    ///
    /// 1. an explicit `font:`, which names a family in `[assets.fonts]`. A
    ///    name nobody declared was already reported when the view was lowered,
    ///    so reaching here with one means the fallback below is the honest
    ///    answer rather than a second, quieter diagnostic every frame.
    /// 2. the family of the `typo:` token the element names. The token has
    ///    carried a family since the theme runtime landed and it has been
    ///    dropped on the floor the whole time, which is exactly what made a
    ///    two-typeface theme impossible to express.
    /// 3. the theme's body family, so a project that declares one gets it
    ///    everywhere without repeating itself.
    ///
    /// `None` at the end is the system font, and it is silent by design: a
    /// project that declares no fonts is not making a mistake.
    pub(super) fn resolve_family(&mut self, attrs: &[Attr]) -> Option<std::sync::Arc<str>> {
        if let Some(name) = self.font_prop(attrs) {
            return self.theme.font(&name).map(|f| f.resolved.clone());
        }
        if let Some(name) = self.typo_family(attrs) {
            return self.theme.font(&name).map(|f| f.resolved.clone());
        }
        self.theme_body_family()
    }

    /// The `font:` an element names, written bare (`font: display`) or as a
    /// string. Both spellings are accepted for the same reason `weight:` takes
    /// both a keyword and a number: neither is more correct, and rejecting one
    /// only makes the language harder to guess at.
    fn font_prop(&mut self, attrs: &[Attr]) -> Option<String> {
        let value = attrs.iter().find_map(|a| match &a.kind {
            AttrKind::Prop { value } if a.name.as_str() == "font" => Some(value.clone()),
            _ => None,
        })?;
        // A bare identifier is read as a *value* first and as a family name
        // second. `font: display` names the family `display`; `font: family`
        // inside a view that takes a `family: Str` parameter means that
        // parameter, and reading it the other way round would make a
        // reusable specimen view impossible to write.
        if let Value::Str(s) = self.eval_pure(&value) {
            return Some(s);
        }
        if let Expr::Ident(sym, _) = &value {
            return Some(sym.as_str().to_string());
        }
        None
    }

    /// The family named by the `typo:` token an element carries, if any.
    fn typo_family(&mut self, attrs: &[Attr]) -> Option<String> {
        let token = self.typo_token_name(attrs)?;
        self.theme.typo(&token)?.family.clone()
    }

    /// The theme typography token a `typo:` names, in either spelling: bare
    /// (`typo: headline`) or through the injected theme (`typo: t.headline`).
    ///
    /// Both have to come here, because the accessor *evaluates* to the token's
    /// size, and a size carries neither family nor weight. Reading only the
    /// bare spelling dropped both on the spelling everyone writes.
    fn typo_token_name(&self, attrs: &[Attr]) -> Option<String> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "typo" => Some(value),
            _ => None,
        })?;
        match value {
            Expr::Ident(sym, _) => Some(sym.as_str().to_string()),
            Expr::Member { base, field, .. } => match base.as_ref() {
                Expr::Ident(name, _) if matches!(self.env.lookup(name), Some(Value::Theme(_))) => {
                    Some(field.as_str().to_string())
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The family the theme's `body` token names, if it names one.
    ///
    /// The default face for everything that says nothing. A theme pairing a
    /// display face with a UI face wants the UI face on ordinary text, and
    /// making every `Text` repeat that is how a type scale stops being used.
    fn theme_body_family(&mut self) -> Option<std::sync::Arc<str>> {
        let name = self.theme.typo("body")?.family.clone()?;
        self.theme.font(&name).map(|f| f.resolved.clone())
    }

    /// Checks every literal `font:` in `view` against the declared families
    /// (RFC-0034).
    ///
    /// Once, when the view is lowered, rather than on every render: a
    /// diagnostic re-reported sixty times a second is a diagnostic nobody
    /// reads. Only a literal name is checked; a computed one is allowed
    /// through, because the check exists to catch typos and not to forbid the
    /// rare deliberate dynamic case, exactly as the anchor check does.
    pub(super) fn check_font_families(&mut self, view: &ViewDecl) {
        let declared: Vec<String> = self.theme.fonts().keys().cloned().collect();
        // A name the view binds is a value reference, not a family literal:
        // `View Specimen(family: Str)` writing `font: family` means the
        // parameter. Treating those as literals would make a reusable
        // type-specimen view unwritable, which is the first thing anyone
        // builds with a font selector. Kept apart from `declared` so a bound
        // name is skipped without ever being offered as a spelling hint.
        let mut bound: Vec<String> = view
            .params
            .iter()
            .map(|p| p.name.as_str().to_string())
            .collect();
        bound.extend(view.body.iter().filter_map(|m| match m {
            Member::Var { name, .. } | Member::Let { name, .. } | Member::Fn { name, .. } => {
                Some(name.as_str().to_string())
            }
            _ => None,
        }));
        Self::walk_font_members(&view.body, &declared, &mut bound, &mut self.errors);
    }

    fn walk_font_members(
        members: &[Member],
        declared: &[String],
        bound: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        for member in members {
            match member {
                Member::Element(el) => Self::walk_font_element(el, declared, bound, errors),
                Member::For {
                    var, index, body, ..
                } => {
                    // The loop variables are in scope for the body and no
                    // further, so they are pushed and popped rather than
                    // collected up front.
                    let depth = bound.len();
                    bound.push(var.as_str().to_string());
                    if let Some(i) = index {
                        bound.push(i.as_str().to_string());
                    }
                    Self::walk_font_members(body, declared, bound, errors);
                    bound.truncate(depth);
                }
                Member::Route { body, .. } => {
                    Self::walk_font_members(body, declared, bound, errors);
                }
                Member::When { then, els, .. } => {
                    Self::walk_font_members(then, declared, bound, errors);
                    if let Some(e) = els {
                        Self::walk_font_members(e, declared, bound, errors);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_font_element(
        el: &ElementNode,
        declared: &[String],
        bound: &mut Vec<String>,
        errors: &mut Vec<CompileError>,
    ) {
        for attr in &el.attrs {
            if attr.name.as_str() != "font" {
                continue;
            }
            let AttrKind::Prop { value } = &attr.kind else {
                continue;
            };
            // Only a name written literally can be checked. Anything computed
            // is allowed through: the check exists to catch typos, not to
            // forbid the deliberate dynamic case, exactly as the anchor check
            // does.
            let (name, span) = match value {
                Expr::Ident(sym, span) => (sym.as_str().to_string(), *span),
                Expr::StrLit(parts, span) => {
                    let [crate::parser::ast::StrPart::Text(name)] = parts.as_slice() else {
                        continue;
                    };
                    (name.clone(), *span)
                }
                _ => continue,
            };
            if bound.contains(&name)
                || declared
                    .iter()
                    .any(|d| *d == name || super::theme::to_camel(d) == name)
            {
                continue;
            }
            errors.push(CompileError::UnknownFontFamily {
                span,
                name: name.clone(),
                hint: crate::util::closest_match(&name, declared.iter().map(String::as_str))
                    .map(str::to_string),
            });
        }
        Self::walk_font_members(&el.children, declared, bound, errors);
    }

    /// Installs `theme` as the active design-token theme and provides it as the
    /// ambient `Theme` so `inject Theme as t` resolves in every view (RFC-0022).
    ///
    /// Creates the reactive scheme signal (a `Bool`, `true` ⇒ dark) seeded from
    /// the theme's active scheme, then provides a [`Value::Theme`] carrying it.
    /// Call once, before [`lower_view`](Self::lower_view). Idempotent: re-calling
    /// reuses the existing scheme signal (so a hot-reload keeps the toggle state).
    pub fn set_theme(&mut self, theme: super::theme::Theme) {
        let dark = theme.active_dark;
        self.theme = theme;
        self.register_theme_fonts();
        // A whole new token set: every resolved colour, size and typo scale
        // may differ, so the next frame rebuilds (RFC-0032 §Q6).
        self.invalidate_retained_layout();
        let sig = if let Some(sig) = self.theme_scheme {
            sig
        } else {
            let sig = self.ctx.create_signal(Value::Bool(dark));
            self.theme_scheme = Some(sig);
            sig
        };
        self.env.provide(Symbol::intern("Theme"), Value::Theme(sig));
        if self.theme_mix.is_none() {
            self.theme_mix = Some(self.ctx.create_signal(Value::Float(1.0)));
            self.theme_mix_value = 1.0;
        }
    }

    /// Loads every family the theme declares into the measurement
    /// `FontSystem` and rebuilds the table the render thread reads
    /// (RFC-0034 §Reference "Asset side"). The measuring and painting font
    /// systems must resolve every family the same way, or layout sizes text for
    /// one face while the GPU draws another.
    ///
    /// This is the single source of truth the invariant asks for. The bytes
    /// come from one place, the theme; they go to the measurer directly
    /// because it lives on this thread, and to the paint `FontSystem` on the
    /// frame, because it does not. Neither side ever reads a font file.
    fn register_theme_fonts(&mut self) {
        if self.theme.fonts().is_empty() {
            // Nothing declared: leave the table empty and, crucially, do not
            // force the lazy measurer into existence. A project with no fonts
            // must not start paying for a `FontSystem` because this ran.
            self.fonts = std::sync::Arc::default();
            return;
        }
        let mut measurer = self.text_measurer.borrow_mut();
        let measurer = measurer.get_or_insert_with(byard_core::text::TextMeasurer::new);
        let mut table = byard_core::frame::FontTable::default();
        for (declared, font) in self.theme.fonts() {
            let here = measurer.register_family(&font.bytes);
            // The manifest resolved this name from the same bytes when it read
            // the file. If the measurer disagrees, one of the two font systems is
            // shaping against a family name the other never produced.
            debug_assert_eq!(
                here.as_deref(),
                Some(font.resolved.as_ref()),
                "family `{declared}` resolves differently on the measurement side"
            );
            table.push(byard_core::frame::FontFace {
                declared: declared.clone(),
                resolved: font.resolved.clone(),
                bytes: font.bytes.clone(),
            });
        }
        self.fonts = std::sync::Arc::new(table);
    }

    /// Flips the active color scheme (RFC-0022 §1): writes the reactive scheme
    /// signal, marking every binding that reads a theme token dirty, and
    /// mirrors the flag into the theme's non-reactive default accessors. The
    /// next [`tick`](Self::tick) recomputes; the next [`render`](Self::render)
    /// paints the new scheme. A no-op if no theme has been installed.
    ///
    /// This is the programmatic entry point (a controller, or a future OS
    /// dark-mode observer) equivalent to `theme.dark = <dark>` in `byld`.
    pub fn set_theme_dark(&mut self, dark: bool) {
        self.theme.active_dark = dark;
        // RFC-0032 §Q6: a scheme flip changes nearly every resolved value, so
        // marking would visit the whole tree and then recompute the whole tree.
        // It is also rare and user-initiated, so one rebuilt frame is
        // imperceptible, and it is the honest default for a change this wide.
        self.invalidate_retained_layout();
        if let Some(sig) = self.theme_scheme {
            self.ctx.write_signal(sig, Value::Bool(dark));
        }
    }

    /// Whether the active theme scheme is currently dark (RFC-0022 §1).
    #[must_use]
    pub fn theme_is_dark(&self) -> bool {
        self.theme_scheme.map_or(self.theme.active_dark, |sig| {
            self.ctx.peek_signal(sig).as_bool().unwrap_or(false)
        })
    }

    /// Resolves the `typo:` prop to a font size in logical pixels (RFC-0005
    /// `Typo`, completed by RFC-0022). Accepts either a bare token
    /// (`typo: titleLarge` → a `Str`, resolved against the theme's typography
    /// then the built-in M3 scale) or a theme accessor (`typo: t.titleLarge` →
    /// an `Int` size projected by [`lower_theme_member`](Self::lower_theme_member)).
    /// The weight of the `typo:` token an element names, if it names one
    /// (RFC-0034).
    fn typo_weight(&mut self, attrs: &[Attr]) -> Option<u16> {
        let token = self.typo_token_name(attrs)?;
        self.theme.typo_weight(&token)
    }

    pub(super) fn eval_typo_size(&mut self, attrs: &[Attr]) -> Option<i64> {
        let value = attrs.iter().find_map(|a| match (&a.name, &a.kind) {
            (n, AttrKind::Prop { value }) if n.as_str() == "typo" => Some(value),
            _ => None,
        })?;
        match self.eval_pure(value) {
            // A theme accessor already resolved to a concrete pixel size.
            Value::Int(px) => Some(px),
            Value::Float(px) =>
            {
                #[allow(clippy::cast_possible_truncation)]
                Some(px as i64)
            }
            // A bare token name → theme typography, falling back to M3 sizes.
            Value::Str(token) => self
                .theme
                .typo_size(&token)
                .or_else(|| super::theme::resolve_typo(&token))
                .map(|s| {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        s as i64
                    }
                }),
            _ => None,
        }
    }

    /// Resolves `theme.dark` (the assignable/bindable scheme flag) to its backing
    /// scheme signal, or `None` if `target` is not that member (RFC-0022 §1).
    pub(super) fn resolve_theme_scheme_target(&self, target: &Expr) -> Option<SignalId> {
        let Expr::Member { base, field, .. } = target else {
            return None;
        };
        if field.as_str() != "dark" {
            return None;
        }
        let Expr::Ident(base_name, _) = base.as_ref() else {
            return None;
        };
        match self.env.lookup(base_name) {
            Some(Value::Theme(sig)) => Some(*sig),
            _ => None,
        }
    }
}
