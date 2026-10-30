"""Split a restaurant bill between friends."""

from decimal import ROUND_HALF_UP, Decimal

CENT = Decimal("0.01")


def split_bill(amount, tip_percent, people):
    """Return what each person pays, tip included, rounded to the cent."""
    total = Decimal(str(amount)) * (1 + Decimal(str(tip_percent)))
    share = total / people
    return share.quantize(CENT, rounding=ROUND_HALF_UP)
