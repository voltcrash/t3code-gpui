//! Networking and child processes run on tokio; views run on GPUI's executor.
//! Futures cross with `spawn`, whose handle any executor can await.

use std::future::Future;

use gpui_kit::{App, Global};

pub struct Tokio(pub tokio::runtime::Runtime);

impl Global for Tokio {}

pub fn spawn<F>(cx: &App, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    cx.global::<Tokio>().0.spawn(future)
}
