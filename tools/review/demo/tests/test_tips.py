import unittest
from decimal import Decimal

from tips import split_bill


class SplitBillTest(unittest.TestCase):
    def test_no_tip_splits_evenly(self):
        self.assertEqual(split_bill(90, 0, 3), Decimal("30.00"))

    def test_tip_is_a_percentage(self):
        self.assertEqual(split_bill(100, 20, 4), Decimal("30.00"))

    def test_shares_are_rounded_to_the_cent(self):
        self.assertEqual(split_bill(10, 0, 3), Decimal("3.33"))


if __name__ == "__main__":
    unittest.main()
