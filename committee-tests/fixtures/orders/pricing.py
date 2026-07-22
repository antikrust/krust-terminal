"""Money is pence. Always pence, always whole numbers.

Nothing in this file changes in the review — it is the contract the two
files that *did* change are supposed to be honouring.
"""


def line_total(unit_pence, quantity):
    """What `quantity` of an item at `unit_pence` each costs, in pence."""
    return unit_pence * quantity


def apply_discount(total_pence, fraction):
    """`total_pence` with `fraction` of it taken off, rounded to whole pence.

    `fraction` is a fraction of one, not a percentage: a fifth off is 0.2.
    """
    return round(total_pence * (1 - fraction))
