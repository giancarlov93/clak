//! REST APIs: from an OpenAPI or by hand, one command per endpoint.
//!
//! To agents, any REST API looks like a connected program, with one command per endpoint
//! (`clak petstore findPetsByStatus --status sold`). This crate holds the part that doesn't
//! depend on the hub: the model (endpoints, parameters, access), the OpenAPI reader (JSON
//! and YAML), the HTTP request with its reply for the agent, and where APIs and their
//! secrets are saved; plus the [`ApiHub`], which registers every enabled API in the hub as
//! a program.

pub mod hub;
pub mod model;
pub mod openapi;
pub mod rest;
pub mod store;
pub mod text;
pub mod yaml;

pub use hub::ApiHub;
pub use model::{
    ApiAuth, ApiDraft, ApiEndpoint, ApiEntry, ApiParameter, AuthKind, BodyKind, Object, ParamIn,
    ParamType,
};
pub use openapi::{ApiImport, OpenApiError};
pub use rest::{AccessToken, StaticToken};
pub use store::ApiStore;
pub use yaml::YamlError;
