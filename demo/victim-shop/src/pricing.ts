export interface Item {
  sku: string;
  price: number;
  quantity: number;
}

/** Price in cents after a percentage discount, rounded half up. */
export function discounted(cents: number, percent: number): number {
  return Math.round((cents * (100 - percent)) / 100);
}
