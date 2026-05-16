"""Synthetic corpus generator for the agent-success bench.

The premise: real LLM SQL agents fail most often because they hallucinate
column names — typos, wrong case, plural-vs-singular, missing/added
underscores. This corpus enumerates those failure modes against a fixed
schema so we can measure resolve_column's contribution in isolation,
without an LLM in the loop.

The result is a JSON list of records:
    {"question": str, "table": str, "gold_column": str, "agent_candidate": str}

`agent_candidate` is what an agent would *likely* emit given the question;
the harness then checks whether the candidate matches a real column
(baseline) or whether resolve_column points it at the gold column
(treatment).
"""

from __future__ import annotations

import json
import pathlib


SCHEMA = {
    "customers": ["id", "email", "first_name", "last_name", "region", "created_at"],
    "orders": ["id", "customer_id", "total_cents", "currency", "status", "placed_at"],
    "products": ["id", "sku", "name", "category", "price_cents", "in_stock"],
    "shipments": ["id", "order_id", "carrier", "tracking_number", "shipped_at", "delivered_at"],
    "reviews": ["id", "product_id", "customer_id", "rating", "body", "submitted_at"],
}

# Pairs of (real column name, plausible agent typo) covering common
# hallucination shapes.
TYPO_RULES = [
    ("id", "Id"),
    ("id", "ID"),
    ("email", "emailadress"),
    ("first_name", "firstname"),
    ("last_name", "lastname"),
    ("region", "regions"),
    ("created_at", "createdAt"),
    ("customer_id", "customerid"),
    ("customer_id", "customer_idd"),
    ("total_cents", "totalcents"),
    ("total_cents", "total"),
    ("currency", "currencies"),
    ("status", "state"),
    ("placed_at", "placedAt"),
    ("sku", "sku_code"),
    ("name", "product_name"),
    ("category", "categories"),
    ("price_cents", "pricecents"),
    ("in_stock", "instock"),
    ("order_id", "orderid"),
    ("carrier", "carrier_name"),
    ("tracking_number", "trackingnumber"),
    ("shipped_at", "shippedAt"),
    ("delivered_at", "deliveredAt"),
    ("product_id", "productid"),
    ("rating", "ratings"),
    ("body", "review_body"),
    ("submitted_at", "submittedAt"),
]


def build() -> list[dict[str, str]]:
    records: list[dict[str, str]] = []
    for table, cols in SCHEMA.items():
        for real, typo in TYPO_RULES:
            if real not in cols:
                continue
            records.append(
                {
                    "question": f"What is the {typo} on {table}?",
                    "table": table,
                    "gold_column": real,
                    "agent_candidate": typo,
                }
            )
    return records


def write_schema_sql(path: pathlib.Path) -> None:
    lines = []
    for table, cols in SCHEMA.items():
        col_defs = ", ".join(f'"{c}" TEXT' for c in cols)
        lines.append(f'CREATE TABLE "{table}" ({col_defs});')
    path.write_text("\n".join(lines))


if __name__ == "__main__":
    here = pathlib.Path(__file__).parent
    corpus = build()
    (here / "synthetic_corpus.json").write_text(json.dumps(corpus, indent=2))
    write_schema_sql(here / "synthetic_schema.sql")
    print(f"wrote {len(corpus)} records")
