//! Attribute macros that mark a function as a `source`, `validate` or `sink`
//! for an identifier, e.g. `#[tinted::source(vote)]`.
//!
//! Each macro inserts a nested function named `__source__<id>`,
//! `__validate__<id>` or `__sink__<id>` and a call to it at the top of the
//! annotated function, so the annotation shows up as an edge in the LLVM call
//! graph. `tinted-check` reads that graph.

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

fn mark(kind: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    let id = attr.to_string();
    assert!(
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "expected #[{kind}(identifier)]"
    );
    let mut tokens: Vec<TokenTree> = item.into_iter().collect();
    let body = match tokens.pop() {
        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => g,
        _ => panic!("#[{kind}] must be applied to a function with a body"),
    };
    // `black_box` gives the marker an effect, so that an optimised build does
    // not delete the call to it.
    let mut new_body: TokenStream = format!(
        "#[inline(never)] fn __{kind}__{id}() {{ ::core::hint::black_box(()); }} __{kind}__{id}();"
    )
    .parse()
    .unwrap();
    new_body.extend(body.stream());
    let mut group = Group::new(Delimiter::Brace, new_body);
    group.set_span(body.span());
    tokens.push(group.into());
    // The annotated function must stay a function of its own: inlined into a
    // caller, its marker would make the caller look annotated instead.
    let mut marked: TokenStream = "#[inline(never)]".parse().unwrap();
    marked.extend(tokens);
    marked
}

#[proc_macro_attribute]
pub fn source(attr: TokenStream, item: TokenStream) -> TokenStream {
    mark("source", attr, item)
}

#[proc_macro_attribute]
pub fn validate(attr: TokenStream, item: TokenStream) -> TokenStream {
    mark("validate", attr, item)
}

#[proc_macro_attribute]
pub fn sink(attr: TokenStream, item: TokenStream) -> TokenStream {
    mark("sink", attr, item)
}
