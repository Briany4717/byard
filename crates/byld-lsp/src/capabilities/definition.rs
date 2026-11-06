//! Goto Definition capability for jumping to declarations of views, variables, parameters, and style rules.

use byard_compiler::interp::intrinsics::lookup;
use lsp_types::{GotoDefinitionResponse, Location, Position};

use crate::semantic::symbols::{PackageIndex, qualified_target};
use crate::state::document::Document;
use crate::syntax::ast_utils::{
    HoverTarget, find_class_ref_at_offset, find_element_ref_at_offset, find_hover_target,
    find_local_declaration_span, span_contains,
};
use crate::syntax::line_index::LineIndex;

/// The view `name` declared in this document, as a location.
fn local_view(doc: &Document, name: &str) -> Option<GotoDefinitionResponse> {
    let view = doc.parsed.views.iter().find(|v| v.name.as_str() == name)?;
    let range = doc.line_index.span_to_range(&doc.content, view.span);
    Some(GotoDefinitionResponse::Scalar(Location::new(
        doc.uri.clone(),
        range,
    )))
}

/// The view `name` of dependency `pkg`, in the file that declares it.
fn imported_view(index: &PackageIndex, pkg: &str, name: &str) -> Option<GotoDefinitionResponse> {
    let (view, path) = index.view_in_file(pkg, name)?;
    let source = std::fs::read_to_string(path).ok()?;
    let range = LineIndex::new(&source).span_to_range(&source, view.span);
    let path = path.canonicalize().ok()?;
    let uri = url::Url::from_file_path(path).ok()?.as_str().parse().ok()?;
    Some(GotoDefinitionResponse::Scalar(Location::new(uri, range)))
}

/// Handles a goto definition request.
#[must_use]
pub fn handle_definition(doc: &Document, pos: Position) -> Option<GotoDefinitionResponse> {
    let offset = doc.line_index.position_to_offset(&doc.content, pos)?;

    if let Some(view_ref_name) = find_element_ref_at_offset(&doc.parsed.views, offset) {
        // A built-in's name is the built-in, even where a package declares a
        // view of that name: that view is reached as `package.Name`.
        let index = PackageIndex::build(doc.file_path().as_deref());
        if let Some((pkg, rest)) = qualified_target(
            &view_ref_name,
            &doc.parsed.imports,
            index.own_package.as_deref(),
        ) {
            match pkg {
                None => return local_view(doc, rest),
                Some(pkg) => return imported_view(&index, &pkg, rest),
            }
        }
        if lookup(&view_ref_name).is_some() {
            return None;
        }
        if let Some(target_view) = doc
            .parsed
            .views
            .iter()
            .find(|v| v.name.as_str() == view_ref_name)
        {
            let range = doc.line_index.span_to_range(&doc.content, target_view.span);
            return Some(GotoDefinitionResponse::Scalar(Location::new(
                doc.uri.clone(),
                range,
            )));
        }
    }

    if let Some(target) = find_hover_target(&doc.parsed.views, offset) {
        match target {
            HoverTarget::VarIdent { name, .. } => {
                let enclosing_view = doc
                    .parsed
                    .views
                    .iter()
                    .find(|v| span_contains(v.span, offset))?;

                for param in &enclosing_view.params {
                    if param.name.as_str() == name {
                        let range = doc.line_index.span_to_range(&doc.content, param.span);
                        return Some(GotoDefinitionResponse::Scalar(Location::new(
                            doc.uri.clone(),
                            range,
                        )));
                    }
                }

                if let Some(def_span) =
                    find_local_declaration_span(&enclosing_view.body, &name, offset)
                {
                    let range = doc.line_index.span_to_range(&doc.content, def_span);
                    return Some(GotoDefinitionResponse::Scalar(Location::new(
                        doc.uri.clone(),
                        range,
                    )));
                }

                if let Some(target_view) = doc.parsed.views.iter().find(|v| v.name.as_str() == name)
                {
                    let range = doc.line_index.span_to_range(&doc.content, target_view.span);
                    return Some(GotoDefinitionResponse::Scalar(Location::new(
                        doc.uri.clone(),
                        range,
                    )));
                }
            }
            HoverTarget::Attribute {
                element_name,
                attr_name,
            } => {
                let target_view = doc
                    .parsed
                    .views
                    .iter()
                    .find(|v| v.name.as_str() == element_name)?;
                let param = target_view
                    .params
                    .iter()
                    .find(|p| p.name.as_str() == attr_name)?;
                let range = doc.line_index.span_to_range(&doc.content, param.span);
                return Some(GotoDefinitionResponse::Scalar(Location::new(
                    doc.uri.clone(),
                    range,
                )));
            }
            HoverTarget::Intrinsic { .. } => {}
        }
    }

    if let Some(class_name) = find_class_ref_at_offset(&doc.parsed.views, offset) {
        let enclosing_view = doc
            .parsed
            .views
            .iter()
            .find(|v| span_contains(v.span, offset))?;
        for member in &enclosing_view.body {
            if let byard_compiler::parser::ast::Member::Style { rules, .. } = member {
                for rule in rules {
                    if rule.class.as_str() == class_name {
                        let range = doc.line_index.span_to_range(&doc.content, rule.span);
                        return Some(GotoDefinitionResponse::Scalar(Location::new(
                            doc.uri.clone(),
                            range,
                        )));
                    }
                }
            }
        }
    }

    None
}
