// Copyright 2026 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

//! Create asynchronous actions to be performed in the background.

use std::future::Future;

/// Yields a task which will run the future on the runtime executor.
pub fn future<X: Into<Y>, Y: 'static>(
    future: impl Future<Output = X> + Send + 'static,
) -> crate::ui::iced::Task<Y> {
    crate::ui::iced::Task::future(async move { future.await.into() })
}

/// Yields a task which will return a message.
pub fn message<X: Send + 'static + Into<Y>, Y: 'static>(message: X) -> crate::ui::iced::Task<Y> {
    future(async move { message.into() })
}
