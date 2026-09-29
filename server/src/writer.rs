//! The single writer thread (DESIGN §5.2): owns the `Engine`; everything that writes `jess.db`
//! runs here, one closure at a time. After each job the new head seq is published on a
//! `watch` channel so connection tasks can stream `Changes` (the DB is the queue).

use crate::engine::Engine;
use std::sync::mpsc;
use tokio::sync::{oneshot, watch};

type Job = Box<dyn FnOnce(&mut Engine, &dyn Fn(&Engine)) + Send>;

#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<Job>,
    pub head: watch::Receiver<u64>,
    pub hlc: watch::Receiver<jess_core::hlc::Hlc>,
}

impl Writer {
    /// Spawns the writer thread. `open` builds the engine on that thread.
    pub fn spawn(open: impl FnOnce() -> Engine + Send + 'static) -> Writer {
        let (tx, rx) = mpsc::channel::<Job>();
        let (htx, hrx) = watch::channel(0u64);
        let (ctx, crx) = watch::channel(jess_core::hlc::Hlc::ZERO);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("jess-writer".into())
            .spawn(move || {
                let mut e = open();
                let _ = htx.send(e.head);
                let _ = ctx.send(e.hlc());
                let _ = ready_tx.send(());
                let publish = |e: &Engine| {
                    let _ = ctx.send(e.hlc());
                    if *htx.borrow() != e.head {
                        let _ = htx.send(e.head);
                    }
                };
                while let Ok(job) = rx.recv() {
                    // The job publishes the new head before replying, so callers never see a stale head.
                    job(&mut e, &publish);
                }
                let _ = e.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                tracing::info!("writer stopped");
            })
            .expect("spawn writer");
        let _ = ready_rx.recv();
        Writer {
            tx,
            head: hrx,
            hlc: crx,
        }
    }

    /// Runs `f` on the writer thread and returns its result.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> R {
        let (otx, orx) = oneshot::channel();
        let job: Job = Box::new(move |e, publish| {
            let r = f(e);
            publish(e);
            let _ = otx.send(r);
        });
        self.tx.send(job).expect("writer thread alive");
        orx.await.expect("writer job completed")
    }

    /// Blocking variant (CLI / tests outside a runtime).
    pub fn call_blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> R {
        let (otx, orx) = std::sync::mpsc::channel();
        let job: Job = Box::new(move |e, publish| {
            let r = f(e);
            publish(e);
            let _ = otx.send(r);
        });
        self.tx.send(job).expect("writer thread alive");
        orx.recv().expect("writer job completed")
    }
}
