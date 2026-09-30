//! Every example renders the view it was written to show.
//!
//! The runner used to render whichever view a file declared first, so an
//! example with a helper view above `Main` (`clip_mask`, `anchored_overlay`)
//! showed that helper alone: four coloured tiles instead of the clip masks,
//! a bare panel instead of the overlay demo. Their own tests rendered `Main`
//! by name and passed, which is how it went unnoticed. This checks the choice
//! the runner actually makes, for every example in the tree.

use byard_compiler::parser::{ast::root_view, parse};
use std::path::PathBuf;

fn examples() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path().join("src/main.byd")))
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

#[test]
fn every_example_with_a_main_renders_main() {
    let files = examples();
    assert!(files.len() > 20, "found the examples: {}", files.len());
    for file in files {
        let parsed = parse(&std::fs::read_to_string(&file).unwrap());
        if !parsed.views.iter().any(|v| v.name.as_str() == "Main") {
            continue;
        }
        let root = root_view(&parsed.views).expect("a view");
        assert_eq!(
            root.name.as_str(),
            "Main",
            "{} would render `{}`",
            file.display(),
            root.name.as_str()
        );
    }
}

/// With nothing named `Main`, the first view is still the root, which is
/// what every example without a `Main` relies on.
#[test]
fn without_a_main_the_first_view_is_the_root() {
    let parsed = parse("View A() { Box {} }\nView B() { Box {} }");
    assert_eq!(root_view(&parsed.views).unwrap().name.as_str(), "A");
}
