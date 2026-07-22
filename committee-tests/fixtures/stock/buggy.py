"""Tiny stock-keeping helpers.

Three planted defects, one per function, and deliberately of three different
kinds — a committee that finds all three has genuinely decorrelated blind
spots rather than one shared instinct. They are listed in ../../README.md;
don't read that first if you want an honest run.

Every one of them is a *wrong value of the right type*, which is the class
the reviewer prompt is aimed at, and every one can be demonstrated by two
lines of python.
"""


def cheapest(prices):
    """The lowest price in the list, or None when the list is empty."""
    best = None
    for price in prices:
        if not best or price < best:
            best = price
    return best


def needs_reorder(stock, threshold):
    """True once stock has fallen to the reorder threshold."""
    return stock < threshold


def restock(item, quantity, log=[]):
    """Records one delivery and returns the log for this call."""
    log.append((item, quantity))
    return log
