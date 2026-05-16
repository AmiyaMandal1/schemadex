from typing import Any

__version__: str

class ResolveResult:
    matched: str | None
    confidence: float
    alternatives: list[tuple[str, float]]

class SchemaCache:
    @staticmethod
    def from_url(
        url: str,
        ttl_seconds: int | None = None,
        cache_dir: str | None = None,
        parallel: bool = True,
        sample_values: bool = False,
        sample_top_k: int | None = None,
        sample_sentinel_threshold: float | None = None,
        sample_rows: int | None = None,
    ) -> SchemaCache: ...
    @staticmethod
    def load(url: str, cache_dir: str | None = None) -> SchemaCache | None: ...
    def list_tables(self) -> list[str]: ...
    def get_table(self, name: str) -> dict[str, Any] | None: ...
    def resolve(self, table: str, candidate: str) -> ResolveResult: ...
    def describe_for_agent(
        self,
        max_tokens: int = 2048,
        hint: str | None = None,
        tables: list[str] | None = None,
        include_samples: bool = True,
        include_foreign_keys: bool = True,
    ) -> tuple[str, int]: ...
    def to_json(self) -> str: ...
    def cache_path(self) -> str: ...
    def fingerprint(self) -> str | None: ...
    def refresh(
        self,
        url: str,
        sample_values: bool = False,
        sample_top_k: int | None = None,
        sample_sentinel_threshold: float | None = None,
        sample_rows: int | None = None,
        parallel: bool = True,
    ) -> tuple[list[str], list[str]]: ...
    def refresh_table(
        self,
        url: str,
        table: str,
        sample_values: bool = False,
        sample_top_k: int | None = None,
        sample_sentinel_threshold: float | None = None,
        sample_rows: int | None = None,
    ) -> tuple[list[str], list[str]]: ...
    def run_sql(
        self,
        url: str,
        sql: str,
        token_budget: int = 1024,
    ) -> tuple[str, int]: ...
