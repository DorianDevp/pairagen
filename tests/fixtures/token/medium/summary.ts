import { Order, Product, StockReport } from "./types";
import { indexBySku, buildReport } from "./inventory";

export interface FulfillmentSummary {
  totalItems: number;
  shortfalls: StockReport[];
}

/**
 * Aggregates every order line, then reports each sku that cannot be fully
 * fulfilled from current stock.
 */
export function summarize(
  orders: Order[],
  products: Product[]
): FulfillmentSummary {
  const index = indexBySku(products);

  const requestedBySku = new Map<string, number>();
  for (const order of orders) {
    for (const item of order.items) {
      const prev = requestedBySku.get(item.sku) ?? 0;
      requestedBySku.set(item.sku, prev + item.quantity);
    }
  }

  const shortfalls: StockReport[] = [];
  let totalItems = 0;
  for (const [sku, requested] of requestedBySku) {
    totalItems += requested;
    const product = index.get(sku)!;
    const report = buildReport(sku, requested, product.stock);
    if (report.shortfall > 0) {
      shortfalls.push(report);
    }
  }

  return { totalItems, shortfalls };
}
