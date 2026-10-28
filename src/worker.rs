//! A thread that runs jobs one at a time, in the order they were sent: for
//! a file that is read, changed and written back whole, where two changes
//! running side by side would each write back a file that did not know about
//! the other.

use std::sync::mpsc::{self, Sender, SyncSender};

enum Message<J> {
    Job(J),
    /// Answer once every job sent before this one is done.
    Barrier(SyncSender<()>),
}

pub struct Ordered<J> {
    name: &'static str,
    /// `None` when the thread could not be started.
    tx: Option<Sender<Message<J>>>,
}

impl<J: Send + 'static> Ordered<J> {
    /// Starts the thread `name`, running each job with `run`. It ends when
    /// this is dropped, which for one kept for the process is at exit.
    pub fn start(name: &'static str, mut run: impl FnMut(J) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<Message<J>>();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                for message in rx {
                    match message {
                        Message::Job(job) => run(job),
                        // The queue is in order, so reaching this means every
                        // job sent before it is done
                        Message::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Self { name, tx: Some(tx) },
            Err(err) => {
                log::error!("failed to start the {name} worker: {err}");
                Self { name, tx: None }
            }
        }
    }

    /// Queues `job`. False when there is no thread to run it.
    pub fn send(&self, job: J) -> bool {
        let sent = self
            .tx
            .as_ref()
            .is_some_and(|tx| tx.send(Message::Job(job)).is_ok());
        if !sent {
            log::warn!("the {} worker is not running", self.name);
        }
        sent
    }

    /// Waits until every job sent so far is done. False when there is no
    /// thread, so nothing sent was done.
    pub fn flush(&self) -> bool {
        let Some(tx) = &self.tx else {
            return false;
        };
        let (done_tx, done_rx) = mpsc::sync_channel(0);
        tx.send(Message::Barrier(done_tx)).is_ok() && done_rx.recv().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::Ordered;
    use std::sync::{Arc, Mutex};

    #[test]
    fn jobs_run_in_order_and_flush_waits_for_them() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let worker = Ordered::start("test", {
            let seen = Arc::clone(&seen);
            move |job: u32| {
                std::thread::sleep(std::time::Duration::from_millis(u64::from(5 - job)));
                seen.lock().unwrap().push(job);
            }
        });
        for job in 0..5 {
            assert!(worker.send(job));
        }
        assert!(worker.flush());
        assert_eq!(*seen.lock().unwrap(), vec![0, 1, 2, 3, 4]);
    }
}
