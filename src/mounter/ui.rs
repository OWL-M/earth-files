//! The mounters as the app's runtime sees them: their answers as tasks and
//! their events as a subscription.

use crate::ui::Task;
use crate::ui::iced::futures::SinkExt;
use crate::ui::iced::{Subscription, stream};

use super::{Answer, MOUNTERS, MounterKey, MounterMessage};

/// `answer` as a task.
pub fn task<T: Send + 'static>(answer: Answer<T>) -> Task<T> {
    Task::future(answer)
}

/// What the mounter `key` reports: its mounts and volumes, first as they
/// are now and again as they change, and how mounts and connections end.
pub fn subscription(key: MounterKey) -> Subscription<MounterMessage> {
    Subscription::run_with(key, |key| {
        let key = *key;
        stream::channel(
            1,
            move |mut output: crate::ui::iced::futures::channel::mpsc::Sender<MounterMessage>| async move {
                if let Some(mounter) = MOUNTERS.get(&key) {
                    mounter.rescan();
                    while let Some(message) = mounter.next_event().await {
                        if output.send(message).await.is_err() {
                            break;
                        }
                    }
                }
                std::future::pending().await
            },
        )
    })
}
