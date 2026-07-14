export interface Product {
  sku: string;
  name: string;
  stock: number;
}

export interface OrderItem {
  sku: string;
  quantity: number;
}

export interface Order {
  id: string;
  items: OrderItem[];
}

export interface StockReport {
  sku: string;
  requested: number;
  available: number;
  shorfall: number;
}
