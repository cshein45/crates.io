#![doc = include_str!("../README.md")]

mod client;
mod jwt;
#[cfg(test)]
mod test_keys;

pub use crate::client::{GitHubApp, GitHubAppClient};

#[cfg(feature = "mock")]
pub use crate::client::MockGitHubApp;
