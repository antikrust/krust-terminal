"""Tiny stock-keeping helpers.

Prices are whole pence. A stock level is a count of items on the shelf.
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
