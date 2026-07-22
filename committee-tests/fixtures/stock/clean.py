"""Tiny stock-keeping helpers.

This is the committed, correct version. `setup.sh` commits it, then
overwrites it with `buggy.py` so the working tree has a diff to review.
"""


def cheapest(prices):
    """The lowest price in the list, or None when the list is empty."""
    best = None
    for price in prices:
        if best is None or price < best:
            best = price
    return best


def needs_reorder(stock, threshold):
    """True once stock has fallen to the reorder threshold."""
    return stock <= threshold


def restock(item, quantity, log=None):
    """Records one delivery and returns the log for this call."""
    if log is None:
        log = []
    log.append((item, quantity))
    return log
