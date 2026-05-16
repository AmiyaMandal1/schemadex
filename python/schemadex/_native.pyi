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
        history: int | None = None,
        max_history: int = 10,
        memoize_results: bool = False,
        memo_capacity: int = 128,
    ) -> SchemaCache: ...
    @staticmethod
    def load(
        url: str,
        cache_dir: str | None = None,
        history: int | None = None,
        max_history: int = 10,
        memoize_results: bool = False,
        memo_capacity: int = 128,
    ) -> SchemaCache | None: ...
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
        include_examples: bool = False,
    ) -> tuple[str, int]: ...
    def examples_for_table(
        self,
        table: str,
        max_examples: int | None = None,
    ) -> list[str]: ...
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
        allow_write: bool = False,
        memoize: bool = False,
    ) -> tuple[str, int]: ...
    def store_embeddings(self, index: dict[str, Any]) -> None: ...
    def load_embeddings(self) -> dict[str, Any] | None: ...
    def snapshot(self) -> str: ...
    def history(self) -> list[tuple[int, list[str]]]: ...
    def invalidate_table(self, table: str) -> None: ...

# Async free functions. Each returns a coroutine awaitable from an asyncio
# event loop. They share the same tokio runtime as the synchronous API.
async def from_url_async(
    url: str,
    ttl_seconds: int | None = None,
    cache_dir: str | None = None,
    parallel: bool = True,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
    history: int | None = None,
    max_history: int = 10,
    memoize_results: bool = False,
    memo_capacity: int = 128,
) -> SchemaCache: ...
async def refresh_async(
    cache: SchemaCache,
    url: str,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
    parallel: bool = True,
) -> tuple[list[str], list[str]]: ...
async def refresh_table_async(
    cache: SchemaCache,
    url: str,
    table: str,
    sample_values: bool = False,
    sample_top_k: int | None = None,
    sample_sentinel_threshold: float | None = None,
    sample_rows: int | None = None,
) -> tuple[list[str], list[str]]: ...
async def run_sql_async(
    cache: SchemaCache,
    url: str,
    sql: str,
    token_budget: int = 1024,
    allow_write: bool = False,
    memoize: bool = False,
) -> tuple[str, int]: ...
def clear_pool_cache() -> None: ...
def pool_size() -> int: ...
