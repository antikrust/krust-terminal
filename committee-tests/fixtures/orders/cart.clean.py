"""Adding a basket up and taking the sale off it."""

import pricing


def checkout(items, percent_off):
    """The total for `items` less `percent_off` percent, in pence.

    `items` are (name, unit_pence, quantity) triples.
    """
    total = sum(pricing.line_total(unit, quantity) for _, unit, quantity in items)
    return pricing.apply_discount(total, percent_off / 100)
