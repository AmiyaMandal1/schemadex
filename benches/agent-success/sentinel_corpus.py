"""Generator for the sentinel-value mini-corpus (v0.3 item 3).

Premise: there is a class of question that an agent cannot answer from
the schema alone — only from the value distribution. The canonical case
is the Nokia 'No Delay' story: 80% of `outages.delay_code` rows are
`'No Delay'`, and an agent asked "what's the dominant delay code?" must
either run a `GROUP BY` query (slow, wasteful) or see the sentinel
schemadex attaches to the column.

This script emits two artifacts beside it:

- ``sentinel_seed.sql``: a single-table schema + 100 INSERT statements
  whose `delay_code` distribution is intentionally skewed
  (80% 'No Delay', 10% 'Backhaul', 5% 'RF', 5% 'Power').
- ``sentinel_corpus.json``: a 5-question corpus probing that
  distribution. Every record has the same gold answer ('No Delay')
  because that's the only sentinel that fires above the 40% threshold.

Run:
    python benches/agent-success/sentinel_corpus.py
"""

from __future__ import annotations

import json
import pathlib


SCHEMA_SQL = (
    "CREATE TABLE outages (\n"
    "    id INTEGER PRIMARY KEY,\n"
    "    region TEXT NOT NULL,\n"
    "    delay_code TEXT NOT NULL\n"
    ");"
)

# Distribution: 80 / 10 / 5 / 5 across 100 rows.
DISTRIBUTION = [
    ("No Delay", 80),
    ("Backhaul", 10),
    ("RF", 5),
    ("Power", 5),
]

REGIONS = ["north", "south", "east", "west"]


QUESTIONS = [
    {
        "question": "What is the most common delay_code on outages?",
        "table": "outages",
        "gold_column": "delay_code",
        "gold_answer": "No Delay",
    },
    {
        "question": (
            "Are there any outages with no delay? If so, what's the dominant code?"
        ),
        "table": "outages",
        "gold_column": "delay_code",
        "gold_answer": "No Delay",
    },
    {
        "question": (
            "Which delay_code value appears most frequently in the outages table?"
        ),
        "table": "outages",
        "gold_column": "delay_code",
        "gold_answer": "No Delay",
    },
    {
        "question": (
            "If I look at outages.delay_code, what is the modal (most common) value?"
        ),
        "table": "outages",
        "gold_column": "delay_code",
        "gold_answer": "No Delay",
    },
    {
        "question": (
            "On the outages table, what value dominates the delay_code column?"
        ),
        "table": "outages",
        "gold_column": "delay_code",
        "gold_answer": "No Delay",
    },
]


def build_seed_sql() -> str:
    rows: list[str] = []
    pk = 1
    for value, count in DISTRIBUTION:
        for i in range(count):
            region = REGIONS[i % len(REGIONS)]
            rows.append(
                f"INSERT INTO outages (id, region, delay_code) VALUES "
                f"({pk}, '{region}', '{value}');"
            )
            pk += 1
    return SCHEMA_SQL + "\n\n" + "\n".join(rows) + "\n"


def main() -> int:
    here = pathlib.Path(__file__).parent
    (here / "sentinel_seed.sql").write_text(build_seed_sql())
    (here / "sentinel_corpus.json").write_text(json.dumps(QUESTIONS, indent=2))
    print(
        f"wrote sentinel_seed.sql ({sum(c for _, c in DISTRIBUTION)} rows) "
        f"and sentinel_corpus.json ({len(QUESTIONS)} questions)"
    )
    return 0


if __name__ == "__main__":
    import sys

    sys.exit(main())
