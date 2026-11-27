//! `byard explain <Name>`: the section of the error index for one error,
//! named as a diagnostic names it (`error[UnknownAttribute]`).

use byard_compiler::explain;

pub fn run(name: &str) -> Result<(), String> {
    let name = name
        .trim_start_matches("error[")
        .trim_start_matches("warning[");
    let name = name.trim_end_matches(']');
    let Some(text) = explain::explain(name) else {
        let hint = explain::closest(name)
            .map(|n| format!(" (did you mean `{n}`?)"))
            .unwrap_or_default();
        return Err(format!(
            "no error is named `{name}`{hint}: the name is the word in brackets, as in `error[UnknownAttribute]`"
        ));
    };
    println!("{}", plain(name, text));
    Ok(())
}

/// A section as terminal text: the name, then the prose, and each example
/// indented under it in place of its fence.
fn plain(name: &str, text: &str) -> String {
    let mut out = format!("{name}\n\n");
    let mut in_code = false;
    for line in text.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str("    ");
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_is_printed_without_its_fences() {
        let text = explain::explain("DuplicateEnumCase").expect("a section");
        let shown = plain("DuplicateEnumCase", text);
        assert!(shown.starts_with("DuplicateEnumCase\n\n"), "{shown}");
        assert!(!shown.contains("```"), "{shown}");
        assert!(shown.contains("    enum A { x, x }"), "{shown}");
    }

    #[test]
    fn a_name_in_its_brackets_or_misspelt_is_understood() {
        assert!(run("error[DuplicateEnumCase]").is_ok());
        let err = run("DuplicateEnumcase").unwrap_err();
        assert!(err.contains("did you mean `DuplicateEnumCase`"), "{err}");
    }
}
