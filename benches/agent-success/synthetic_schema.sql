CREATE TABLE "customers" ("id" TEXT, "email" TEXT, "first_name" TEXT, "last_name" TEXT, "region" TEXT, "created_at" TEXT);
CREATE TABLE "orders" ("id" TEXT, "customer_id" TEXT, "total_cents" TEXT, "currency" TEXT, "status" TEXT, "placed_at" TEXT);
CREATE TABLE "products" ("id" TEXT, "sku" TEXT, "name" TEXT, "category" TEXT, "price_cents" TEXT, "in_stock" TEXT);
CREATE TABLE "shipments" ("id" TEXT, "order_id" TEXT, "carrier" TEXT, "tracking_number" TEXT, "shipped_at" TEXT, "delivered_at" TEXT);
CREATE TABLE "reviews" ("id" TEXT, "product_id" TEXT, "customer_id" TEXT, "rating" TEXT, "body" TEXT, "submitted_at" TEXT);