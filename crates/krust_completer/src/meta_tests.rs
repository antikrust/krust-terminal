use super::*;

/*
0 1 2 3
w a r p
-------
0     4  << the span for the string "krust" is (0, 4)

Spanned {
    item: String::new("krust"),  << krust string
    span: Span::new(0, 4)       << span
}

or >> String::new("krust").spanned(Span::new(0, 4))        */
fn krust() -> Spanned<String> {
    String::from("krust").spanned(Span::new(0, 4))
}

fn empty() -> Spanned<String> {
    String::new().spanned_unknown()
}

#[test]
fn knows_distances() {
    assert!(krust().span.distance() == 4);
    assert!(empty().span.distance() == 0);
}
