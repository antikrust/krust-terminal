use super::*;

/*
0 1 2 3
w a r p
-------
0     4  << the span for the string "bork" is (0, 4)

Spanned {
    item: String::new("bork"),  << bork string
    span: Span::new(0, 4)       << span
}

or >> String::new("bork").spanned(Span::new(0, 4))        */
fn bork() -> Spanned<String> {
    String::from("bork").spanned(Span::new(0, 4))
}

fn empty() -> Spanned<String> {
    String::new().spanned_unknown()
}

#[test]
fn knows_distances() {
    assert!(bork().span.distance() == 4);
    assert!(empty().span.distance() == 0);
}
