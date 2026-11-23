use std::sync::atomic::{AtomicU64, Ordering};

use tower_lsp_server::Client;
use tower_lsp_server::ls_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
    notification::Progress, request::WorkDoneProgressCreate,
};

static NEXT_TOKEN_ID: AtomicU64 = AtomicU64::new(1);

/// Standard LSP Work Done Progress reporter (`window/workDoneProgress/create` + `$/progress`).
pub struct ProgressHandle {
    client: Client,
    token: NumberOrString,
    ended: bool,
}

impl ProgressHandle {
    pub fn allocate_token() -> NumberOrString {
        let id = NEXT_TOKEN_ID.fetch_add(1, Ordering::Relaxed);
        NumberOrString::String(format!("flutter-debugger-{id}"))
    }

    pub async fn begin(client: Client, title: impl Into<String>, message: Option<String>) -> Self {
        let token = Self::allocate_token();
        Self::begin_with_token(client, token, title, message).await
    }

    pub async fn begin_with_token(
        client: Client,
        token: NumberOrString,
        title: impl Into<String>,
        message: Option<String>,
    ) -> Self {
        let _ = client
            .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .await;

        client
            .send_notification::<Progress>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: title.into(),
                        cancellable: Some(false),
                        message,
                        percentage: None,
                    },
                )),
            })
            .await;

        Self {
            client,
            token,
            ended: false,
        }
    }

    pub async fn report(&self, message: impl Into<String>) {
        if self.ended {
            return;
        }
        self.client
            .send_notification::<Progress>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                    WorkDoneProgressReport {
                        cancellable: Some(false),
                        message: Some(message.into()),
                        percentage: None,
                    },
                )),
            })
            .await;
    }

    pub async fn end(mut self, message: Option<String>) {
        self.ended = true;
        Self::end_token(&self.client, self.token.clone(), message).await;
    }

    pub async fn end_token(client: &Client, token: NumberOrString, message: Option<String>) {
        client
            .send_notification::<Progress>(ProgressParams {
                token,
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message,
                })),
            })
            .await;
    }
}
