//! schemadex-core: schema introspection and resolution toolkit for SQL agents.
//!
//! See the workspace README for the full pitch. This crate exposes:
//! - [`SchemaIntrospector`] trait for live database introspection
//! - Backend implementations behind feature flags (`postgres`, `sqlite`, `duckdb_backend`)
//! - [`SchemaCache`] for on-disk caching with DDL fingerprinting
//! - Fuzzy column resolution and agent-facing describe API

pub mod agent;
pub mod cache;
pub mod error;
pub mod fingerprint;
pub mod introspector;
pub mod model;
pub mod resolve;
pub mod sampling;

pub mod backends;

pub use crate::agent::{describe_for_agent, DescribeOptions};
pub use crate::cache::SchemaCache;
pub use crate::error::{Result, SchemadexError};
pub use crate::introspector::{Backend, SchemaIntrospector};
pub use crate::model::{
    Column, ColumnSample, DataType, Database, ForeignKey, PrimaryKey, SampleStats, Table,
};
pub use crate::resolve::{resolve_column, ResolveResult};
