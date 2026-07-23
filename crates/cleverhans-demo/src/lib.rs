//! Library surface of the demo: the registry (schema + handlers) is exposed
//! so integration tests and the eval suite can build it without the server.

pub mod generated;
pub mod host;
pub mod registry;
