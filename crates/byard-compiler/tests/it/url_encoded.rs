//! `text.urlEncoded()`: text made safe to put in a URL.
//!
//! A request is written as a string with values interpolated into it, and
//! there was no way to make a value safe: a `&` or a `#` typed into a search
//! field cut the query short, and nothing said so.

use byard_compiler::interp::eval::Interpreter;
use byard_compiler::parser::parse;
use byard_core::frame::RenderFrame;

/// What `Text("/s?name={q.urlEncoded()}&count=5")` draws for `q`.
fn url_for(q: &str) -> String {
    let src = format!(
        "View Main() {{
    var q: Str = \"{q}\"
    Text(\"/s?name={{q.urlEncoded()}}&count=5\")
}}"
    );
    let parsed = parse(&src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let checked = byard_compiler::infer::check_views(&parsed.views).errors;
    assert!(checked.is_empty(), "the type checker: {checked:?}");
    let mut interp = Interpreter::new();
    interp.load_views(&parsed.views);
    let tree = interp.lower_view(&parsed.views[0], &["Main"]);
    assert!(interp.errors().is_empty(), "{:?}", interp.errors());
    interp.tick();
    let mut frame = RenderFrame::new();
    interp.render(&tree, &mut frame, 800.0, 200.0);
    frame.texts()[0].text.clone()
}

#[test]
fn plain_text_is_left_alone() {
    assert_eq!(url_for("Lisbon"), "/s?name=Lisbon&count=5");
    assert_eq!(url_for("a-b_c.d~e9"), "/s?name=a-b_c.d~e9&count=5");
}

#[test]
fn what_would_end_the_value_is_encoded() {
    assert_eq!(url_for("a&b"), "/s?name=a%26b&count=5");
    assert_eq!(url_for("a#b"), "/s?name=a%23b&count=5");
    assert_eq!(url_for("a=b?c/d"), "/s?name=a%3Db%3Fc%2Fd&count=5");
    assert_eq!(url_for("50%"), "/s?name=50%25&count=5");
    assert_eq!(url_for("a+b"), "/s?name=a%2Bb&count=5");
}

#[test]
fn a_space_is_percent_twenty() {
    assert_eq!(url_for("New York"), "/s?name=New%20York&count=5");
}

#[test]
fn other_scripts_are_encoded_byte_by_byte() {
    assert_eq!(url_for("São"), "/s?name=S%C3%A3o&count=5");
    assert_eq!(url_for("東京"), "/s?name=%E6%9D%B1%E4%BA%AC&count=5");
}

#[test]
fn the_empty_string_stays_empty() {
    assert_eq!(url_for(""), "/s?name=&count=5");
}

/// It is a method of text and of nothing else, and a misspelling is told.
#[test]
fn the_checker_knows_it() {
    let errors = |body: &str| {
        let parsed = parse(&format!("View Main() {{\n{body}\n}}"));
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        format!(
            "{:?}",
            byard_compiler::infer::check_views(&parsed.views).errors
        )
    };
    let misspelt = errors("var q: Str = \"a\"\nText(q.urlEncode())");
    assert!(misspelt.contains("urlEncoded"), "a suggestion: {misspelt}");
    let on_a_list = errors("var xs: List<Int> = [1]\nText(\"{xs.urlEncoded()}\")");
    assert!(on_a_list.contains("UnknownMethod"), "{on_a_list}");
}
