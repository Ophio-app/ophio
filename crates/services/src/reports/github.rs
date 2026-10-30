// SPDX-License-Identifier: Apache-2.0
use crate::{CompanionHub, hub::Inner};
use ports::github::{Github, GithubError, Method};
use protocol::reports::Trail;
use serde_json::Value;
use std::sync::{Arc, Weak};

pub(crate) struct Calls {
    pub inner: Arc<dyn Github>,
    pub hub: Weak<Inner>,
}
impl std::fmt::Debug for Calls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubCalls").finish_non_exhaustive()
    }
}
impl Calls {
    fn record(&self, operation: &str, result: &Result<Value, GithubError>) {
        if let Some(inner) = self.hub.upgrade() {
            let status = match result {
                Ok(_) => 200,
                Err(e) => e.status().unwrap_or(0),
            };
            (CompanionHub { inner }).report_trail(Trail::Github {
                operation: operation.into(),
                status,
            });
        }
    }
}
#[async_trait::async_trait]
impl Github for Calls {
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, GithubError> {
        let operation = match method {
            Method::Get => "rest_get",
            Method::Post => "rest_post",
            Method::Patch => "rest_patch",
            Method::Put => "rest_put",
            Method::Delete => "rest_delete",
        };
        let result = self.inner.rest(method, path, body).await;
        self.record(operation, &result);
        result
    }
    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, GithubError> {
        let result = self.inner.graphql(query, variables).await;
        self.record("graphql", &result);
        result
    }
}
