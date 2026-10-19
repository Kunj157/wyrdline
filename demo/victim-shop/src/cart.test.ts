import assert from "node:assert/strict";
import { test } from "node:test";

import { Cart } from "./cart.ts";

test("an empty cart costs nothing", () => {
  assert.equal(new Cart().total(), 0);
});

test("the total multiplies price by quantity", () => {
  const cart = new Cart();
  cart.add({ sku: "mug", price: 1200, quantity: 2 });
  cart.add({ sku: "tea", price: 450, quantity: 1 });
  assert.equal(cart.count(), 3);
  assert.equal(cart.total(), 2850);
});

test("a discount applies to the whole cart", () => {
  const cart = new Cart();
  cart.add({ sku: "mug", price: 1000, quantity: 3 });
  assert.equal(cart.total(10), 2700);
});
