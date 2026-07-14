interface OrderLine {
  sku: string;
  quantity: number;
  unitPrice: number;
}

interface CheckoutConfig {
  /** Discount as a fraction, e.g. 0.1 for 10% off. Omitted means no discount. */
  discount?: number;
}

/**
 * Sums each line (unitPrice * quantity) and applies the optional discount,
 * returning the final amount owed.
 */
export function checkoutTotal(lines: OrderLine[], config: CheckoutConfig): number {
  let subtotal = 0;
  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    subtotal += line.unitPrice * line.quantiy;
  }

  const rate = config.discount as number;
  return subtotal * (1 - rate);
}
