import { discounted, type Item } from "./pricing.ts";

export class Cart {
  private items: Item[] = [];

  add(item: Item): void {
    this.items.push(item);
  }

  count(): number {
    return this.items.reduce((n, i) => n + i.quantity, 0);
  }

  /** Total in cents. Bug: the quantity of each line is ignored. */
  total(discountPercent = 0): number {
    const sum = this.items.reduce((s, i) => s + i.price, 0);
    return discounted(sum, discountPercent);
  }
}
