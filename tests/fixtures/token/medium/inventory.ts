import { Product, StockReport } from "./types";

export function indexBySku(products: Product[]): Map<string, Product> {
  const index = new Map<string, Product>();
  for (const product of products) {
    index.set(product.sku, product);
  }
  return index;
}

/**
 * Builds a stock report for one sku: how many units short we are, given the
 * requested quantity and the quantity currently available.
 */
export function buildReport(
  sku: string,
  requested: number,
  available: number
): StockReport {
  return {
    sku,
    requested,
    available,
    shortfall: available - requested,
  };
}
