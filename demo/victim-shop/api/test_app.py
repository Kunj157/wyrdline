import unittest

from app import total


class TotalTest(unittest.TestCase):
    def test_multiplies_quantity(self):
        self.assertEqual(total([{"price": 1200, "quantity": 2}]), 2400)


if __name__ == "__main__":
    unittest.main()
