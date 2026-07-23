use super::*;

/*
0 1 2 3
w a r p
-------
0     4  << the span for the string "derp" is (0, 4)

Spanned {
    item: String::new("derp"),  << derp string
    span: Span::new(0, 4)       << span
}

or >> String::new("derp").spanned(Span::new(0, 4))        */
fn derp() -> Spanned<String> {
    String::from("derp").spanned(Span::new(0, 4))
}

fn empty() -> Spanned<String> {
    String::new().spanned_unknown()
}

#[test]
fn knows_distances() {
    assert!(derp().span.distance() == 4);
    assert!(empty().span.distance() == 0);
}
