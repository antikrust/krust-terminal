"""Rendering an invoice for a human, who thinks in pounds."""

import pricing


def render_line(name, unit_pence, quantity):
    """One line of an invoice, money shown in pounds."""
    total = pricing.line_total(unit_pence, quantity)
    return f"{name}: £{total:.2f}"
